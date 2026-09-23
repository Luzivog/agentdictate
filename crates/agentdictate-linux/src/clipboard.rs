//! Owns the X11 CLIPBOARD and PRIMARY selections in-process for every
//! delivery, whether the target is an X11 window or a native Wayland one.
//!
//! A selection's owner serves its contents on demand, so one long-lived
//! thread keeps an unmapped window and answers other applications' requests
//! for the published text. Mutter's XWayland selection bridge carries both
//! selections to Wayland-native applications. wl-clipboard is deliberately
//! not used: GNOME offers no data-control protocol, so every `wl-copy` maps
//! a transient toplevel that visibly re-layouts the taskbar at paste time.
//!
//! Because the owner answers every request itself, it also sees the target
//! take the text: a request that arrives after the paste key press
//! acknowledges the paste. Clipboard managers, Mutter's own included, fetch
//! the text as soon as ownership changes, which is before the chord.

use std::{
    fmt,
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError,
        mpsc::{self, RecvTimeoutError, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use x11rb::{
    COPY_FROM_PARENT, CURRENT_TIME, NONE,
    connection::{Connection, RequestConnection as _},
    errors::ReplyError,
    protocol::{
        Event,
        xproto::{
            Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask,
            PropMode, SELECTION_NOTIFY_EVENT, SelectionClearEvent, SelectionNotifyEvent,
            SelectionRequestEvent, Window, WindowClass,
        },
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        CLIPBOARD,
        TARGETS,
        TEXT,
        UTF8_STRING,
    }
}

/// Bytes of a ChangeProperty request ahead of its data.
const CHANGE_PROPERTY_HEADER: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardSelection {
    /// What Ctrl+V pastes, and Shift+Insert in most applications.
    Clipboard,
    /// What middle-click pastes, and Shift+Insert in many terminals.
    Primary,
}

const SELECTIONS: [ClipboardSelection; 2] =
    [ClipboardSelection::Primary, ClipboardSelection::Clipboard];

#[derive(Debug)]
pub enum ClipboardError {
    /// No X server was reachable, or one of its requests failed.
    Display(String),
    /// The text needs more than one X request; INCR transfers are not
    /// implemented because transcripts stay far below the limit.
    TooLarge { bytes: usize, limit: usize },
    /// Another application owned the selection right after AgentDictate
    /// claimed it.
    NotOwned(ClipboardSelection),
    /// The owner thread has exited, usually because the X server went away.
    Stopped,
    /// The X server did not answer before the delivery deadline.
    Deadline,
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Display(error) => write!(formatter, "X server unavailable: {error}"),
            Self::TooLarge { bytes, limit } => write!(
                formatter,
                "the text is {bytes} bytes, more than one X request carries ({limit})"
            ),
            Self::NotOwned(selection) => {
                write!(
                    formatter,
                    "another application took the {selection:?} selection"
                )
            }
            Self::Stopped => formatter.write_str("the clipboard owner stopped"),
            Self::Deadline => formatter.write_str("the X server did not answer in time"),
        }
    }
}

impl std::error::Error for ClipboardError {}

/// Publishes text on X selections and serves it until the next publication
/// or until another application takes the selection. The owner thread and
/// its X connection start with the first publication and are replaced when
/// the X server went away since.
#[derive(Default)]
pub struct SelectionOwner {
    worker: Option<Worker>,
}

impl SelectionOwner {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Serves `text` on exactly `selections`, releasing any other selection
    /// this owner still holds. Returns once the X server reports AgentDictate
    /// as the owner of every requested selection.
    pub fn publish(
        &mut self,
        text: &str,
        selections: &[ClipboardSelection],
        deadline: Instant,
    ) -> Result<(), ClipboardError> {
        if let Some(worker) = &self.worker {
            match worker.claim(text, selections, deadline) {
                Err(ClipboardError::Stopped) => self.worker = None,
                result => return result,
            }
        }
        let worker = self.worker.insert(Worker::start(deadline)?);
        worker.claim(text, selections, deadline)
    }

    /// Waits until `until` for an application to request the published text
    /// at or after `since`, the paste key press, and returns when it did.
    pub fn text_requested_since(&self, since: Instant, until: Instant) -> Option<Instant> {
        let shared = &self.worker.as_ref()?.shared;
        let (published, _) = shared
            .text_requested
            .wait_timeout_while(
                lock(&shared.published),
                until.saturating_duration_since(Instant::now()),
                |published| !published.requested_since(since),
            )
            .unwrap_or_else(PoisonError::into_inner);
        published
            .last_text_request
            .filter(|requested| *requested >= since)
    }
}

/// The handle to one owner thread and its X connection.
struct Worker {
    connection: Arc<RustConnection>,
    window: Window,
    commands: mpsc::Sender<Command>,
    shared: Arc<Shared>,
}

/// What the owner thread serves requests from and the paste waits on.
#[derive(Default)]
struct Shared {
    published: Mutex<Published>,
    text_requested: Condvar,
}

enum Command {
    Claim {
        text: String,
        selections: Vec<ClipboardSelection>,
        reply: mpsc::SyncSender<Result<(), ClipboardError>>,
    },
    Stop,
}

impl Worker {
    /// Connects on the new thread, so an unresponsive X server costs at most
    /// the time until `deadline`.
    fn start(deadline: Instant) -> Result<Self, ClipboardError> {
        let shared = Arc::new(Shared::default());
        let (commands, received) = mpsc::channel();
        let (ready, started) = mpsc::sync_channel(1);
        let served = Arc::clone(&shared);
        thread::Builder::new()
            .name("agentdictate-clipboard".into())
            .spawn(move || match Owner::connect() {
                Ok(owner) => {
                    if ready
                        .send(Ok((Arc::clone(&owner.connection), owner.window)))
                        .is_ok()
                    {
                        owner.serve(&served, &received);
                    }
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            })
            .map_err(|error| ClipboardError::Display(error.to_string()))?;
        let (connection, window) = started
            .recv_timeout(remaining(deadline))
            .map_err(owner_unresponsive)??;
        Ok(Self {
            connection,
            window,
            commands,
            shared,
        })
    }

    fn claim(
        &self,
        text: &str,
        selections: &[ClipboardSelection],
        deadline: Instant,
    ) -> Result<(), ClipboardError> {
        let (reply, replied) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Claim {
                text: text.to_owned(),
                selections: selections.to_vec(),
                reply,
            })
            .map_err(|_| ClipboardError::Stopped)?;
        self.wake()?;
        replied
            .recv_timeout(remaining(deadline))
            .map_err(owner_unresponsive)?
    }

    /// Interrupts the owner thread's wait for X events so it reads its
    /// commands: a client message to a window reaches the window's creator.
    fn wake(&self) -> Result<(), ClipboardError> {
        let wake = ClientMessageEvent::new(32, self.window, AtomEnum::NONE, [0_u32; 5]);
        self.connection
            .send_event(false, self.window, EventMask::NO_EVENT, wake)
            .and_then(|_| self.connection.flush())
            .map_err(|_| ClipboardError::Stopped)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // The thread exits and closes its connection, and the X server then
        // clears the selections it owned.
        if self.commands.send(Command::Stop).is_ok() {
            let _ = self.wake();
        }
    }
}

fn owner_unresponsive(error: RecvTimeoutError) -> ClipboardError {
    match error {
        RecvTimeoutError::Timeout => ClipboardError::Deadline,
        RecvTimeoutError::Disconnected => ClipboardError::Stopped,
    }
}

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn failed(error: impl fmt::Display) -> ClipboardError {
    ClipboardError::Display(error.to_string())
}

fn lock(published: &Mutex<Published>) -> MutexGuard<'_, Published> {
    published.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The owner thread's X connection and its unmapped window.
struct Owner {
    connection: Arc<RustConnection>,
    window: Window,
    atoms: Atoms,
}

impl Owner {
    fn connect() -> Result<Self, ClipboardError> {
        let (connection, screen) = x11rb::connect(None).map_err(failed)?;
        let root = connection.setup().roots[screen].root;
        let window = connection.generate_id().map_err(failed)?;
        connection
            .create_window(
                0,
                window,
                root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                COPY_FROM_PARENT,
                &CreateWindowAux::new(),
            )
            .map_err(failed)?;
        let atoms = Atoms::new(&connection)
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        Ok(Self {
            connection: Arc::new(connection),
            window,
            atoms,
        })
    }

    /// Answers X events until the connection fails or the handle is dropped.
    fn serve(&self, shared: &Shared, commands: &mpsc::Receiver<Command>) {
        loop {
            let served = match self.connection.wait_for_event() {
                Ok(Event::SelectionRequest(request)) => self.answer(shared, &request),
                Ok(Event::SelectionClear(clear)) => self.cleared(shared, &clear),
                Ok(Event::ClientMessage(_)) => match self.run_commands(shared, commands) {
                    Some(()) => Ok(()),
                    None => return,
                },
                Ok(Event::Error(error)) => {
                    // Usually a requestor window destroyed before its reply.
                    tracing::debug!(?error, "clipboard owner request failed");
                    Ok(())
                }
                Ok(_) => Ok(()),
                Err(error) => Err(error.into()),
            };
            if let Err(error) = served {
                tracing::warn!(%error, "clipboard owner lost its X connection");
                return;
            }
        }
    }

    /// Runs the queued commands. Returns `None` once the handle is gone.
    fn run_commands(&self, shared: &Shared, commands: &mpsc::Receiver<Command>) -> Option<()> {
        loop {
            match commands.try_recv() {
                Ok(Command::Claim {
                    text,
                    selections,
                    reply,
                }) => {
                    let _ = reply.send(self.claim(shared, text, &selections));
                }
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => return None,
                Err(TryRecvError::Empty) => return Some(()),
            }
        }
    }

    fn claim(
        &self,
        shared: &Shared,
        text: String,
        selections: &[ClipboardSelection],
    ) -> Result<(), ClipboardError> {
        check_size(text.len(), self.connection.maximum_request_bytes())?;
        for selection in selections {
            self.connection
                .set_selection_owner(self.window, self.atoms.selection(*selection), CURRENT_TIME)
                .map_err(failed)?;
        }
        // Every owner is read back: claimed selections must be ours, and a
        // selection left out may still be ours from an earlier publication.
        // Requests wait on this thread, so none sees a half-updated state.
        let mut served = Vec::new();
        for selection in SELECTIONS {
            let atom = self.atoms.selection(selection);
            let owner = self
                .connection
                .get_selection_owner(atom)
                .map_err(failed)?
                .reply()
                .map_err(failed)?
                .owner;
            match (selections.contains(&selection), owner == self.window) {
                (true, true) => served.push(selection),
                (false, true) => {
                    self.connection
                        .set_selection_owner(NONE, atom, CURRENT_TIME)
                        .map_err(failed)?;
                }
                (true, false) | (false, false) => {}
            }
        }
        self.connection.flush().map_err(failed)?;
        lock(&shared.published).publish(text, &served);
        match selections
            .iter()
            .find(|selection| !served.contains(selection))
        {
            Some(lost) => Err(ClipboardError::NotOwned(*lost)),
            None => Ok(()),
        }
    }

    fn answer(&self, shared: &Shared, request: &SelectionRequestEvent) -> Result<(), ReplyError> {
        let answer = lock(&shared.published).answer(&self.atoms, request, Instant::now());
        shared.text_requested.notify_all();
        // Obsolete clients name no property; ICCCM replies on the target.
        let property = match request.property {
            NONE => request.target,
            property => property,
        };
        let property = match answer {
            Answer::Refuse => NONE,
            Answer::Targets(targets) => {
                self.connection.change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    AtomEnum::ATOM,
                    &targets,
                )?;
                property
            }
            Answer::Text { type_, bytes } => {
                self.connection.change_property8(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    type_,
                    &bytes,
                )?;
                property
            }
        };
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property,
        };
        self.connection
            .send_event(false, request.requestor, EventMask::NO_EVENT, notify)?;
        self.connection.flush()?;
        Ok(())
    }

    /// Stops serving a selection another application took. A clear can be
    /// stale when AgentDictate claimed the selection again since, so the
    /// current owner decides.
    fn cleared(&self, shared: &Shared, clear: &SelectionClearEvent) -> Result<(), ReplyError> {
        let Some(selection) = self.atoms.selection_named(clear.selection) else {
            return Ok(());
        };
        let owner = self
            .connection
            .get_selection_owner(clear.selection)?
            .reply()?
            .owner;
        if owner != self.window {
            lock(&shared.published).lose(selection);
        }
        Ok(())
    }
}

impl Atoms {
    fn selection(&self, selection: ClipboardSelection) -> Atom {
        match selection {
            ClipboardSelection::Clipboard => self.CLIPBOARD,
            ClipboardSelection::Primary => AtomEnum::PRIMARY.into(),
        }
    }

    fn selection_named(&self, atom: Atom) -> Option<ClipboardSelection> {
        SELECTIONS
            .into_iter()
            .find(|selection| self.selection(*selection) == atom)
    }
}

/// One ChangeProperty request carries the whole text. With BIG-REQUESTS
/// (XWayland has it) the limit is 16 MiB, far above any transcript, so
/// larger text is refused instead of implementing INCR transfers.
fn check_size(bytes: usize, maximum_request_bytes: usize) -> Result<(), ClipboardError> {
    let limit = maximum_request_bytes.saturating_sub(CHANGE_PROPERTY_HEADER);
    if bytes > limit {
        Err(ClipboardError::TooLarge { bytes, limit })
    } else {
        Ok(())
    }
}

/// The published text and the selections serving it.
#[derive(Debug, Default)]
struct Published {
    text: String,
    clipboard: bool,
    primary: bool,
    /// When the latest request for the text itself, not its targets, came.
    last_text_request: Option<Instant>,
}

/// How the owner answers one selection request.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Refuse,
    Targets(Vec<Atom>),
    Text { type_: Atom, bytes: Vec<u8> },
}

impl Published {
    /// Serves `text` on exactly `served`, forgetting any earlier request.
    fn publish(&mut self, text: String, served: &[ClipboardSelection]) {
        self.text = text;
        self.clipboard = served.contains(&ClipboardSelection::Clipboard);
        self.primary = served.contains(&ClipboardSelection::Primary);
        self.last_text_request = None;
    }

    fn serves(&self, selection: ClipboardSelection) -> bool {
        match selection {
            ClipboardSelection::Clipboard => self.clipboard,
            ClipboardSelection::Primary => self.primary,
        }
    }

    /// Another application took `selection`. The text is dropped once no
    /// selection serves it.
    fn lose(&mut self, selection: ClipboardSelection) {
        match selection {
            ClipboardSelection::Clipboard => self.clipboard = false,
            ClipboardSelection::Primary => self.primary = false,
        }
        if !self.clipboard && !self.primary {
            self.text.clear();
        }
    }

    fn requested_since(&self, since: Instant) -> bool {
        self.last_text_request
            .is_some_and(|requested| requested >= since)
    }

    /// Offers the text as UTF-8 (`UTF8_STRING`, and `TEXT` in the owner's
    /// choice of encoding) and as Latin-1 `STRING`, and records when the
    /// text itself was requested.
    fn answer(&mut self, atoms: &Atoms, request: &SelectionRequestEvent, now: Instant) -> Answer {
        let owned = atoms
            .selection_named(request.selection)
            .is_some_and(|selection| self.serves(selection));
        let string = Atom::from(AtomEnum::STRING);
        let answer = match request.target {
            _ if !owned => Answer::Refuse,
            target if target == atoms.TARGETS => {
                return Answer::Targets(vec![atoms.TARGETS, atoms.UTF8_STRING, atoms.TEXT, string]);
            }
            target if target == atoms.UTF8_STRING || target == atoms.TEXT => Answer::Text {
                type_: atoms.UTF8_STRING,
                bytes: self.text.as_bytes().to_vec(),
            },
            target if target == string => Answer::Text {
                type_: string,
                bytes: latin1(&self.text),
            },
            _ => Answer::Refuse,
        };
        if matches!(answer, Answer::Text { .. }) {
            self.last_text_request = Some(now);
        }
        answer
    }
}

/// ICCCM's `STRING` is ISO Latin-1; characters outside it become `?`.
fn latin1(text: &str) -> Vec<u8> {
    text.chars()
        .map(|character| u8::try_from(character).unwrap_or(b'?'))
        .collect()
}

#[cfg(test)]
mod tests {
    use x11rb::protocol::xproto::SELECTION_REQUEST_EVENT;

    use super::*;

    const ATOMS: Atoms = Atoms {
        CLIPBOARD: 300,
        TARGETS: 301,
        TEXT: 302,
        UTF8_STRING: 303,
    };
    const IMAGE_PNG: Atom = 304;
    // Predefined atoms, identical on every X server.
    const PRIMARY: Atom = 1;
    const STRING: Atom = 31;

    fn request(selection: Atom, target: Atom) -> SelectionRequestEvent {
        SelectionRequestEvent {
            response_type: SELECTION_REQUEST_EVENT,
            sequence: 0,
            time: CURRENT_TIME,
            owner: 7,
            requestor: 9,
            selection,
            target,
            property: 400,
        }
    }

    fn published(text: &str, served: &[ClipboardSelection]) -> Published {
        let mut published = Published::default();
        published.publish(text.to_owned(), served);
        published
    }

    fn text(type_: Atom, bytes: &[u8]) -> Answer {
        Answer::Text {
            type_,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn targets_offer_the_text_encodings_without_acknowledging_a_paste() {
        let mut published = published("Déjà vu", &[ClipboardSelection::Clipboard]);

        assert_eq!(
            published.answer(
                &ATOMS,
                &request(ATOMS.CLIPBOARD, ATOMS.TARGETS),
                Instant::now()
            ),
            Answer::Targets(vec![ATOMS.TARGETS, ATOMS.UTF8_STRING, ATOMS.TEXT, STRING])
        );
        assert_eq!(published.last_text_request, None);
    }

    #[test]
    fn text_is_served_as_utf8_or_latin1_by_target() {
        let mut published = published(
            "Déjà vu €5",
            &[ClipboardSelection::Clipboard, ClipboardSelection::Primary],
        );
        let now = Instant::now();

        for target in [ATOMS.UTF8_STRING, ATOMS.TEXT] {
            assert_eq!(
                published.answer(&ATOMS, &request(PRIMARY, target), now),
                text(ATOMS.UTF8_STRING, "Déjà vu €5".as_bytes())
            );
        }
        assert_eq!(
            published.answer(&ATOMS, &request(ATOMS.CLIPBOARD, STRING), now),
            text(STRING, b"D\xe9j\xe0 vu ?5")
        );
        assert_eq!(
            published.answer(&ATOMS, &request(ATOMS.CLIPBOARD, IMAGE_PNG), now),
            Answer::Refuse
        );
    }

    #[test]
    fn only_the_published_selections_serve_the_text() {
        let mut published = published("transcript", &[ClipboardSelection::Primary]);
        published.publish("copied".to_owned(), &[ClipboardSelection::Clipboard]);
        let now = Instant::now();

        assert_eq!(
            published.answer(&ATOMS, &request(ATOMS.CLIPBOARD, ATOMS.UTF8_STRING), now),
            text(ATOMS.UTF8_STRING, b"copied")
        );
        assert_eq!(
            published.answer(&ATOMS, &request(PRIMARY, ATOMS.UTF8_STRING), now),
            Answer::Refuse
        );
    }

    #[test]
    fn a_selection_another_application_took_is_refused_and_the_text_dropped_with_the_last() {
        let mut published = published(
            "transcript",
            &[ClipboardSelection::Clipboard, ClipboardSelection::Primary],
        );
        let now = Instant::now();

        published.lose(ClipboardSelection::Primary);
        assert_eq!(
            published.answer(&ATOMS, &request(PRIMARY, ATOMS.UTF8_STRING), now),
            Answer::Refuse
        );
        assert_eq!(
            published.answer(&ATOMS, &request(ATOMS.CLIPBOARD, ATOMS.UTF8_STRING), now),
            text(ATOMS.UTF8_STRING, b"transcript")
        );

        published.lose(ClipboardSelection::Clipboard);
        assert_eq!(
            published.answer(&ATOMS, &request(ATOMS.CLIPBOARD, ATOMS.UTF8_STRING), now),
            Answer::Refuse
        );
        assert!(published.text.is_empty());
    }

    #[test]
    fn only_a_text_request_after_the_key_press_acknowledges_the_paste() {
        let mut published = published("transcript", &[ClipboardSelection::Clipboard]);
        let published_at = Instant::now();
        let key_pressed = published_at + Duration::from_millis(25);

        // A clipboard manager fetches as soon as ownership changes.
        published.answer(
            &ATOMS,
            &request(ATOMS.CLIPBOARD, ATOMS.UTF8_STRING),
            published_at + Duration::from_millis(2),
        );
        assert!(!published.requested_since(key_pressed));

        published.answer(
            &ATOMS,
            &request(ATOMS.CLIPBOARD, ATOMS.TARGETS),
            key_pressed + Duration::from_millis(3),
        );
        assert!(!published.requested_since(key_pressed));

        published.answer(
            &ATOMS,
            &request(ATOMS.CLIPBOARD, ATOMS.UTF8_STRING),
            key_pressed + Duration::from_millis(4),
        );
        assert!(published.requested_since(key_pressed));

        published.publish(
            "next transcript".to_owned(),
            &[ClipboardSelection::Clipboard],
        );
        assert!(!published.requested_since(key_pressed));
    }

    #[test]
    fn text_beyond_one_x_request_is_refused() {
        let maximum_request_bytes = 262_140;
        let limit = maximum_request_bytes - CHANGE_PROPERTY_HEADER;

        assert!(check_size(limit, maximum_request_bytes).is_ok());
        assert!(matches!(
            check_size(limit + 1, maximum_request_bytes),
            Err(ClipboardError::TooLarge { bytes, limit: refused_limit })
                if bytes == limit + 1 && refused_limit == limit
        ));
    }
}

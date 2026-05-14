from __future__ import annotations

from .gtk import Gtk


class UiWidgetMixin:
    def _tab_box(self) -> Gtk.Box:
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        box.set_border_width(8)
        return box

    def _grid(self) -> Gtk.Grid:
        grid = Gtk.Grid(row_spacing=8, column_spacing=10)
        grid.set_column_homogeneous(False)
        return grid

    def _grid_attach(self, grid: Gtk.Grid, label: str, widget: Gtk.Widget, row: int) -> None:
        label_widget = Gtk.Label(label=label)
        label_widget.set_xalign(0)
        label_widget.set_valign(Gtk.Align.CENTER)
        grid.attach(label_widget, 0, row, 1, 1)
        if isinstance(widget, Gtk.Switch):
            widget.set_halign(Gtk.Align.START)
            widget.set_valign(Gtk.Align.CENTER)
            widget.set_hexpand(False)
            widget.set_vexpand(False)
        grid.attach(widget, 1, row, 1, 1)

    def _value_label(
        self, container: Gtk.Container, label: str, row: int | None = None
    ) -> Gtk.Label:
        value = Gtk.Label(label="")
        value.set_xalign(0)
        label_widget = Gtk.Label(label=label)
        label_widget.set_xalign(0)
        if isinstance(container, Gtk.Grid):
            assert row is not None
            container.attach(label_widget, 0, row, 1, 1)
            container.attach(value, 1, row, 1, 1)
        else:
            row_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
            row_box.pack_start(label_widget, False, False, 0)
            row_box.pack_start(value, True, True, 0)
            container.pack_start(row_box, False, False, 0)
        return value

    def _combo(self, items: list[str]) -> Gtk.ComboBoxText:
        combo = Gtk.ComboBoxText()
        for item in items:
            combo.append_text(item)
        combo.set_active(0)
        return combo

    def _text_view(self, height: int, editable: bool = True) -> Gtk.ScrolledWindow:
        view = Gtk.TextView()
        view.set_wrap_mode(Gtk.WrapMode.WORD_CHAR)
        view.set_editable(editable)
        view.set_monospace(False)
        scrolled = Gtk.ScrolledWindow()
        scrolled.set_policy(Gtk.PolicyType.AUTOMATIC, Gtk.PolicyType.AUTOMATIC)
        scrolled.set_size_request(-1, height)
        scrolled.add(view)
        scrolled.text_view = view  # type: ignore[attr-defined]
        return scrolled

    def _text_buffer(self, scrolled: Gtk.ScrolledWindow) -> Gtk.TextBuffer:
        view = scrolled.text_view  # type: ignore[attr-defined]
        return view.get_buffer()

    def _set_text(self, scrolled: Gtk.ScrolledWindow, text: str) -> None:
        self._text_buffer(scrolled).set_text(text or "")

    def _get_text(self, scrolled: Gtk.ScrolledWindow) -> str:
        buffer = self._text_buffer(scrolled)
        start, end = buffer.get_bounds()
        return buffer.get_text(start, end, True)

    def _scrolled(self, child: Gtk.Widget, height: int) -> Gtk.ScrolledWindow:
        scrolled = Gtk.ScrolledWindow()
        scrolled.set_policy(Gtk.PolicyType.AUTOMATIC, Gtk.PolicyType.AUTOMATIC)
        scrolled.set_size_request(-1, height)
        scrolled.add(child)
        return scrolled

    def _warning_label(self, text: str) -> Gtk.Label:
        label = Gtk.Label(label=text)
        label.set_line_wrap(True)
        label.set_xalign(0)
        return label

    def _save_button_row(self, box: Gtk.Box) -> None:
        button = Gtk.Button(label="Save settings")
        button.connect("clicked", self._save_from_ui)
        box.pack_end(button, False, False, 0)

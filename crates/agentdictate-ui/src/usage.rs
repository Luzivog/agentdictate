#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UsagePeriod {
    Last7Days,
    #[default]
    Last30Days,
    AllTime,
}

impl UsagePeriod {
    pub const ALL: [Self; 3] = [Self::Last7Days, Self::Last30Days, Self::AllTime];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Last7Days => "Last 7 days",
            Self::Last30Days => "Last 30 days",
            Self::AllTime => "All time",
        }
    }

    pub const fn slug(self) -> &'static str {
        match self {
            Self::Last7Days => "7-days",
            Self::Last30Days => "30-days",
            Self::AllTime => "all-time",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UsageTotals {
    pub dictations: u64,
    pub words: u64,
    pub audio_seconds: u64,
    pub estimated_cost_usd: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageDayViewModel {
    pub label: String,
    pub dictations: u64,
    pub words: u64,
    pub audio_seconds: u64,
    pub estimated_cost_usd: f64,
}

impl UsageDayViewModel {
    pub fn new(
        label: impl Into<String>,
        dictations: u64,
        words: u64,
        audio_seconds: u64,
        estimated_cost_usd: f64,
    ) -> Self {
        Self {
            label: label.into(),
            dictations,
            words,
            audio_seconds,
            estimated_cost_usd,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageViewModel {
    pub period: UsagePeriod,
    pub totals: UsageTotals,
    pub activity: Vec<UsageDayViewModel>,
}

impl UsageViewModel {
    pub fn new(period: UsagePeriod, totals: UsageTotals, activity: Vec<UsageDayViewModel>) -> Self {
        Self {
            period,
            totals,
            activity,
        }
    }

    pub fn dictations_value(&self) -> String {
        format_integer(self.totals.dictations)
    }

    pub fn words_value(&self) -> String {
        format_integer(self.totals.words)
    }

    pub fn audio_value(&self) -> String {
        let minutes = self.totals.audio_seconds / 60;
        let seconds = self.totals.audio_seconds % 60;
        format!("{minutes}m {seconds:02}s")
    }

    pub fn cost_value(&self) -> String {
        format_usd(self.totals.estimated_cost_usd)
    }

    pub fn average_wpm_value(&self) -> String {
        let average_wpm = if self.totals.audio_seconds == 0 {
            0
        } else {
            ((self.totals.words as f64 * 60.0) / self.totals.audio_seconds as f64).round() as u64
        };
        average_wpm.to_string()
    }

    pub fn peak_audio_seconds(&self) -> u64 {
        self.activity
            .iter()
            .map(|day| day.audio_seconds)
            .max()
            .unwrap_or(0)
    }
}

/// Formats an estimated cost; OpenAI bills in US dollars.
pub(crate) fn format_usd(value: f64) -> String {
    format!("${:.2}", value.max(0.0))
}

impl Default for UsageViewModel {
    fn default() -> Self {
        Self::new(UsagePeriod::default(), UsageTotals::default(), Vec::new())
    }
}

fn format_integer(value: u64) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(character);
    }
    formatted
}

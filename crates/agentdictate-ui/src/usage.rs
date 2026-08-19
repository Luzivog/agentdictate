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
    pub currency: String,
}

impl UsageViewModel {
    pub fn new(period: UsagePeriod, totals: UsageTotals, activity: Vec<UsageDayViewModel>) -> Self {
        Self {
            period,
            totals,
            activity,
            currency: "USD".to_owned(),
        }
    }

    pub fn with_currency(mut self, currency: impl Into<String>) -> Self {
        let currency = currency.into();
        let currency = currency.trim();
        self.currency = if currency.is_empty() {
            "USD".to_owned()
        } else {
            currency.to_uppercase()
        };
        self
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
        self.format_cost(self.totals.estimated_cost_usd)
    }

    pub fn average_wpm_value(&self) -> String {
        let average_wpm = if self.totals.audio_seconds == 0 {
            0
        } else {
            ((self.totals.words as f64 * 60.0) / self.totals.audio_seconds as f64).round() as u64
        };
        average_wpm.to_string()
    }

    pub fn format_cost(&self, value: f64) -> String {
        format_currency_amount(&self.currency, value)
    }

    pub fn peak_dictations(&self) -> u64 {
        self.activity
            .iter()
            .map(|day| day.dictations)
            .max()
            .unwrap_or(0)
    }

    /// Keeps short ranges fully labeled while limiting longer charts to six
    /// stable ticks, including both endpoints.
    pub fn axis_label_visible(&self, index: usize) -> bool {
        let count = self.activity.len();
        if index >= count {
            return false;
        }
        if count <= 7 {
            return true;
        }
        if index == count - 1 {
            return true;
        }
        let intervals = 5;
        let step = (count - 1).div_ceil(intervals);
        index.is_multiple_of(step)
    }
}

pub(crate) fn format_currency_amount(currency: &str, value: f64) -> String {
    let value = value.max(0.0);
    match currency {
        "USD" => format!("${value:.2}"),
        "EUR" => format!("€{value:.2}"),
        "GBP" => format!("£{value:.2}"),
        currency => format!("{currency} {value:.2}"),
    }
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

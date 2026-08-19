#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplacementRuleViewModel {
    pub id: i64,
    pub source: String,
    pub replacement: String,
    pub enabled: bool,
    pub case_sensitive: bool,
    pub whole_word_only: bool,
}

impl ReplacementRuleViewModel {
    pub fn new(
        id: i64,
        source: impl Into<String>,
        replacement: impl Into<String>,
        enabled: bool,
        case_sensitive: bool,
        whole_word_only: bool,
    ) -> Self {
        Self {
            id,
            source: source.into(),
            replacement: replacement.into(),
            enabled,
            case_sensitive,
            whole_word_only,
        }
    }

    pub const fn match_policy_label(&self) -> &'static str {
        match (self.case_sensitive, self.whole_word_only) {
            (false, true) => "Whole words",
            (false, false) => "Anywhere",
            (true, true) => "Case-sensitive · Whole words",
            (true, false) => "Case-sensitive · Anywhere",
        }
    }

    pub fn draft(&self) -> ReplacementDraft {
        ReplacementDraft::from_rule(self)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplacementsViewModel {
    pub rules: Vec<ReplacementRuleViewModel>,
}

impl ReplacementsViewModel {
    pub const fn new(rules: Vec<ReplacementRuleViewModel>) -> Self {
        Self { rules }
    }

    pub fn rule_count(&self) -> u64 {
        self.rules.len() as u64
    }

    pub fn enabled_count(&self) -> u64 {
        self.rules.iter().filter(|rule| rule.enabled).count() as u64
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplacementDraft {
    pub source: String,
    pub replacement: String,
    pub enabled: bool,
    pub case_sensitive: bool,
    pub whole_word_only: bool,
}

impl ReplacementDraft {
    pub fn new(source: impl Into<String>, replacement: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            replacement: replacement.into(),
            enabled: true,
            case_sensitive: false,
            whole_word_only: true,
        }
    }

    pub fn from_rule(rule: &ReplacementRuleViewModel) -> Self {
        Self {
            source: rule.source.clone(),
            replacement: rule.replacement.clone(),
            enabled: rule.enabled,
            case_sensitive: rule.case_sensitive,
            whole_word_only: rule.whole_word_only,
        }
    }

    pub fn is_valid(&self) -> bool {
        !self.source.trim().is_empty() && !self.replacement.trim().is_empty()
    }
}

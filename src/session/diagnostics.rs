use crate::models::CodeAnalysis;

/// Parser-only counters used to distinguish valid empty sessions from schema drift.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ParseDiagnostics {
    pub source_records: usize,
    pub recognized_records: usize,
    pub unrecognized_records: usize,
    pub relevant_records: usize,
    pub normalized_records: usize,
    pub failed_relevant_records: usize,
    pub malformed_records: usize,
}

impl ParseDiagnostics {
    pub fn merge(&mut self, other: Self) {
        self.source_records += other.source_records;
        self.recognized_records += other.recognized_records;
        self.unrecognized_records += other.unrecognized_records;
        self.relevant_records += other.relevant_records;
        self.normalized_records += other.normalized_records;
        self.failed_relevant_records += other.failed_relevant_records;
        self.malformed_records += other.malformed_records;
    }

    pub fn record_recognized_source(&mut self) {
        self.source_records += 1;
        self.recognized_records += 1;
    }

    pub fn record_relevant(&mut self, normalized: bool) {
        self.relevant_records += 1;
        if normalized {
            self.normalized_records += 1;
        } else {
            self.failed_relevant_records += 1;
        }
    }

    pub fn record_unrecognized(&mut self) {
        self.source_records += 1;
        self.unrecognized_records += 1;
    }

    pub fn record_malformed(&mut self) {
        self.source_records += 1;
        self.malformed_records += 1;
    }

    pub fn is_complete_failure(&self) -> bool {
        (self.source_records > 0 && self.recognized_records == 0)
            || (self.relevant_records > 0 && self.normalized_records == 0)
    }

    pub fn partial_failure_count(&self) -> usize {
        if self.is_complete_failure() {
            0
        } else {
            self.failed_relevant_records + self.malformed_records + self.unrecognized_records
        }
    }

    pub fn should_emit_session(&self) -> bool {
        self.source_records > 0 && self.recognized_records > 0 && !self.is_complete_failure()
    }
}

/// A normalized analysis value plus parser-only diagnostics.
#[derive(Debug)]
pub(crate) struct ParsedAnalysis {
    pub analysis: CodeAnalysis,
    pub diagnostics: ParseDiagnostics,
}

/// Database-backed analysis row with a stable source identity for ordering.
pub(crate) struct DatabaseAnalysisRow {
    pub source_id: String,
    pub date: String,
    pub analysis: CodeAnalysis,
}

impl ParsedAnalysis {
    pub fn new(analysis: CodeAnalysis, diagnostics: ParseDiagnostics) -> Self {
        Self {
            analysis,
            diagnostics,
        }
    }
}

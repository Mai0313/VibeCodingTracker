/// Trait for rows that can be displayed in tables
pub trait DisplayRow {
    /// Get the date string for this row
    fn date(&self) -> &str;

    /// Get the model name for this row
    fn model(&self) -> &str;

    /// Convert this row to a vector of formatted strings for display
    fn to_display_cells(&self) -> Vec<String>;

    /// Generate a unique key for tracking updates
    fn row_key(&self) -> String {
        format!("{}:{}", self.date(), self.model())
    }
}

/// Trait for daily statistics that can be calculated per provider
pub trait DailyStats: Default + Clone {
    /// Get the number of days with data for this provider
    fn days_count(&self) -> usize;

    /// Calculate all average metrics for this provider
    /// Returns a vector of formatted average values
    fn average_metrics(&self) -> Vec<String>;
}

/// Trait for provider-specific average display configuration
pub trait ProviderDisplay<'a, T: DailyStats> {
    /// Get the display label for this provider
    fn label(&self) -> &'static str;

    /// Get the icon for this provider
    fn icon(&self) -> &'static str;

    /// Get whether this provider should be emphasized (e.g., "Overall")
    fn emphasize(&self) -> bool;

    /// Get the statistics for this provider
    fn stats(&self) -> &'a T;
}

use crate::models::DateUsageResult;
use serde_json::Value;

/// Display usage data as a table
pub fn display_usage_table(usage_data: &DateUsageResult) {
    if usage_data.is_empty() {
        println!("⚠️  No usage data found in Claude Code or Codex sessions");
        return;
    }

    // Collect and sort dates
    let mut dates: Vec<&String> = usage_data.keys().collect();
    dates.sort();

    // Collect rows
    let mut rows = Vec::new();
    let mut totals = UsageRow::default();

    for date in &dates {
        if let Some(date_usage) = usage_data.get(*date) {
            // Sort models
            let mut models: Vec<&String> = date_usage.keys().collect();
            models.sort();

            for model in models {
                if let Some(usage) = date_usage.get(model) {
                    let row = extract_usage_row(date, model, usage);

                    // Accumulate totals
                    totals.input_tokens += row.input_tokens;
                    totals.output_tokens += row.output_tokens;
                    totals.cache_read += row.cache_read;
                    totals.cache_creation += row.cache_creation;
                    totals.total += row.total;

                    rows.push(row);
                }
            }
        }
    }

    // Print table
    print_table_header();
    print_table_separator();

    for row in rows {
        print_table_row(&row);
    }

    print_table_separator();
    print_table_totals(&totals);
    println!();
}

#[derive(Default)]
struct UsageRow {
    date: String,
    model: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read: i64,
    cache_creation: i64,
    total: i64,
}

fn extract_usage_row(date: &str, model: &str, usage: &Value) -> UsageRow {
    let mut row = UsageRow {
        date: date.to_string(),
        model: model.to_string(),
        ..Default::default()
    };

    if let Some(usage_obj) = usage.as_object() {
        // Claude usage
        if let Some(input) = usage_obj.get("input_tokens").and_then(|v| v.as_i64()) {
            row.input_tokens = input;
        }
        if let Some(output) = usage_obj.get("output_tokens").and_then(|v| v.as_i64()) {
            row.output_tokens = output;
        }
        if let Some(cache_read) = usage_obj
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_i64())
        {
            row.cache_read = cache_read;
        }
        if let Some(cache_creation) = usage_obj
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_i64())
        {
            row.cache_creation = cache_creation;
        }
        row.total = row.input_tokens + row.output_tokens + row.cache_read + row.cache_creation;

        // Codex usage
        if let Some(total_usage) = usage_obj
            .get("total_token_usage")
            .and_then(|v| v.as_object())
        {
            if let Some(input) = total_usage.get("input_tokens").and_then(|v| v.as_i64()) {
                row.input_tokens = input;
            }
            if let Some(output) = total_usage.get("output_tokens").and_then(|v| v.as_i64()) {
                row.output_tokens += output;
            }
            if let Some(reasoning) = total_usage
                .get("reasoning_output_tokens")
                .and_then(|v| v.as_i64())
            {
                row.output_tokens += reasoning;
            }
            if let Some(cache_read) = total_usage
                .get("cached_input_tokens")
                .and_then(|v| v.as_i64())
            {
                row.cache_read = cache_read;
            }
            if let Some(total) = total_usage.get("total_tokens").and_then(|v| v.as_i64()) {
                row.total = total;
            }
        }
    }

    row
}

fn print_table_header() {
    println!(
        "{:<12} {:<40} {:>15} {:>15} {:>15} {:>15} {:>15}",
        "Date", "Model", "Input Tokens", "Output Tokens", "Cache Read", "Cache Creation", "Total"
    );
}

fn print_table_separator() {
    println!("{}", "━".repeat(152));
}

fn print_table_row(row: &UsageRow) {
    println!(
        "{:<12} {:<40} {:>15} {:>15} {:>15} {:>15} {:>15}",
        row.date,
        row.model,
        format_number(row.input_tokens),
        format_number(row.output_tokens),
        format_number(row.cache_read),
        format_number(row.cache_creation),
        format_number(row.total),
    );
}

fn print_table_totals(totals: &UsageRow) {
    println!(
        "{:<12} {:<40} {:>15} {:>15} {:>15} {:>15} {:>15}",
        "",
        "TOTAL",
        format_number(totals.input_tokens),
        format_number(totals.output_tokens),
        format_number(totals.cache_read),
        format_number(totals.cache_creation),
        format_number(totals.total),
    );
}

fn format_number(n: i64) -> String {
    if n == 0 {
        "0".to_string()
    } else {
        let s = n.to_string();
        let mut result = String::new();
        let chars: Vec<char> = s.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            if i > 0 && (chars.len() - i) % 3 == 0 {
                result.push(',');
            }
            result.push(*c);
        }
        result
    }
}

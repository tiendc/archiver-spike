//! The report, in Markdown: printed as it is made and saved at the end.

use std::fmt::Write;

#[derive(Default)]
pub struct Report {
    md: String,
}

impl Report {
    pub fn heading(&mut self, text: &str) {
        let _ = writeln!(self.md, "\n## {text}\n");
        println!("\n== {text}");
    }

    pub fn line(&mut self, text: impl AsRef<str>) {
        let _ = writeln!(self.md, "{}", text.as_ref());
        println!("{}", text.as_ref());
    }

    pub fn table(&mut self, head: &[&str], rows: &[Vec<String>]) {
        let _ = writeln!(self.md, "| {} |", head.join(" | "));
        let _ = writeln!(self.md, "|{}", "---|".repeat(head.len()));
        for row in rows {
            let cells: Vec<String> = row.iter().map(|c| c.replace('|', "\\|")).collect();
            let _ = writeln!(self.md, "| {} |", cells.join(" | "));
            println!("  {}", row.join("  ·  "));
        }
        self.md.push('\n');
    }

    pub fn finish(self) -> String {
        self.md
    }
}

/// A mark for a table cell.
pub fn yes(ok: bool) -> String {
    if ok { "✅".into() } else { "❌".into() }
}

/// A stored name as it can be shown: control characters made visible.
pub fn shown(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '\0' => "\\0".to_string(),
            c if c.is_control() => format!("\\u{{{:x}}}", c as u32),
            c => c.to_string(),
        })
        .collect()
}

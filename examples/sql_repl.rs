//! Interactive SQL REPL for the alt-rating SQLite cache.
//!
//! ```text
//! cargo run --example sql_repl
//! cargo run --example sql_repl -- --db path/to.db
//! ```
//!
//! Enter SQL terminated by `;`. Meta-commands: `.help`, `.tables`, `.schema [table]`,
//! `.quit` / `.exit`.

use rusqlite::{Connection, types::ValueRef};
use std::env;
use std::error::Error;
use std::io::{self, Write};
use std::path::PathBuf;

const DEFAULT_DB_PATH: &str = "cache/alt-rating.db";

fn main() -> Result<(), Box<dyn Error>> {
    let db_path = parse_db_path(env::args().skip(1))?;
    if !db_path.exists() {
        return Err(format!(
            "database not found: {} (run the main app first, or pass --db)",
            db_path.display()
        )
        .into());
    }

    let conn = Connection::open(&db_path)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    println!("alt-rating SQL REPL");
    println!("db: {}", db_path.display());
    println!("Type SQL ending with `;`, or `.help` for meta-commands.\n");

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut buffer = String::new();

    loop {
        let prompt = if buffer.is_empty() { "sql> " } else { "...> " };
        write!(stdout, "{prompt}")?;
        stdout.flush()?;

        let mut line = String::new();
        let n = stdin.read_line(&mut line)?;
        if n == 0 {
            println!();
            break;
        }

        let trimmed = line.trim();
        if buffer.is_empty() {
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with('.') {
                if handle_meta(&conn, trimmed)? {
                    break;
                }
                continue;
            }
        }

        buffer.push_str(&line);
        if !buffer.contains(';') {
            continue;
        }

        let sql = buffer.trim().trim_end_matches(';').trim().to_string();
        buffer.clear();
        if sql.is_empty() {
            continue;
        }

        if let Err(err) = run_sql(&conn, &sql) {
            eprintln!("error: {err}");
        }
    }

    Ok(())
}

fn parse_db_path(mut args: impl Iterator<Item = String>) -> Result<PathBuf, Box<dyn Error>> {
    let mut path = PathBuf::from(DEFAULT_DB_PATH);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db" => {
                let value = args.next().ok_or("--db requires a path argument")?;
                path = PathBuf::from(value);
            }
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }
    Ok(path)
}

fn print_usage() {
    println!(
        "Usage: cargo run --example sql_repl [-- --db PATH]

Options:
  --db PATH   SQLite database path (default: {DEFAULT_DB_PATH})
  -h, --help  Show this help"
    );
}

/// Returns `true` when the REPL should exit.
fn handle_meta(conn: &Connection, cmd: &str) -> Result<bool, Box<dyn Error>> {
    let mut parts = cmd.split_whitespace();
    let name = parts.next().unwrap_or("");
    match name {
        ".quit" | ".exit" | ".q" => Ok(true),
        ".help" | ".h" => {
            println!(
                "Meta-commands:
  .help              Show this help
  .tables            List tables
  .schema [table]    Show CREATE statements
  .quit / .exit      Leave the REPL

SQL statements must end with `;`."
            );
            Ok(false)
        }
        ".tables" => {
            list_tables(conn)?;
            Ok(false)
        }
        ".schema" => {
            match parts.next() {
                Some(table) => show_schema(conn, Some(table))?,
                None => show_schema(conn, None)?,
            }
            Ok(false)
        }
        other => {
            eprintln!("unknown meta-command: {other} (try .help)");
            Ok(false)
        }
    }
}

fn list_tables(conn: &Connection) -> Result<(), Box<dyn Error>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )?;
    let names = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut any = false;
    for name in names {
        println!("{}", name?);
        any = true;
    }
    if !any {
        println!("(no tables)");
    }
    Ok(())
}

fn show_schema(conn: &Connection, table: Option<&str>) -> Result<(), Box<dyn Error>> {
    let mut any = false;
    match table {
        Some(name) => {
            let mut stmt = conn.prepare(
                "SELECT sql FROM sqlite_master
                 WHERE sql IS NOT NULL AND name = ?1
                 ORDER BY type, name",
            )?;
            let rows = stmt.query_map([name], |row| row.get::<_, String>(0))?;
            for row in rows {
                println!("{};", row?);
                any = true;
            }
            if !any {
                println!("(no schema for {name})");
            }
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT sql FROM sqlite_master
                 WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%'
                 ORDER BY type, name",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for row in rows {
                println!("{};", row?);
                any = true;
            }
            if !any {
                println!("(no schema)");
            }
        }
    }
    Ok(())
}

fn run_sql(conn: &Connection, sql: &str) -> Result<(), Box<dyn Error>> {
    let trimmed = sql.trim_start();
    let kind = trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();

    match kind.as_str() {
        "select" | "with" | "pragma" | "explain" | "values" => run_query(conn, sql),
        _ => {
            let changed = conn.execute(sql, [])?;
            println!("ok ({changed} row(s) affected)");
            Ok(())
        }
    }
}

fn run_query(conn: &Connection, sql: &str) -> Result<(), Box<dyn Error>> {
    let mut stmt = conn.prepare(sql)?;
    let column_count = stmt.column_count();
    if column_count == 0 {
        let mut rows = stmt.raw_query();
        let mut count = 0usize;
        while rows.next()?.is_some() {
            count += 1;
        }
        println!("({count} row(s))");
        return Ok(());
    }

    let headers: Vec<String> = (0..column_count)
        .map(|i| stmt.column_name(i).unwrap_or("?").to_string())
        .collect();

    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    let mut table: Vec<Vec<String>> = Vec::new();

    let mut rows = stmt.raw_query();
    while let Some(row) = rows.next()? {
        let mut cells = Vec::with_capacity(column_count);
        for i in 0..column_count {
            let cell = format_value(row.get_ref(i)?);
            widths[i] = widths[i].max(cell.len());
            cells.push(cell);
        }
        table.push(cells);
    }

    print_separator(&widths);
    print_row(&headers, &widths);
    print_separator(&widths);
    for cells in &table {
        print_row(cells, &widths);
    }
    print_separator(&widths);
    println!("({} row(s))", table.len());
    Ok(())
}

fn format_value(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => "NULL".into(),
        ValueRef::Integer(v) => v.to_string(),
        ValueRef::Real(v) => v.to_string(),
        ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned(),
        ValueRef::Blob(v) => format!("<blob {} bytes>", v.len()),
    }
}

fn print_separator(widths: &[usize]) {
    print!("+");
    for w in widths {
        print!("{}+", "-".repeat(w + 2));
    }
    println!();
}

fn print_row(cells: &[String], widths: &[usize]) {
    print!("|");
    for (cell, w) in cells.iter().zip(widths) {
        print!(" {cell:<w$} |");
    }
    println!();
}

//! Row/cell mapping plus the shared column-row parser.
//!
//! Pure move from `database::sqlserver` (slice E, step 2): no query text,
//! timeout value, SET option, or permission check changes. Covers
//! `cell_to_json` + `format_numeric_value` + `bytes_to_hex` +
//! `unsupported_sentinel` + `parse_column_rows`.

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use serde_json::{json, Value};
use tiberius::numeric::Numeric;

use crate::database::ColumnInfo;

/// Shared column-row parser so `describe_table` and `describe_table_full`
/// map INFORMATION_SCHEMA.COLUMNS identically.
pub(crate) fn parse_column_rows(rows: &[tiberius::Row]) -> Vec<ColumnInfo> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if let (Some(column), Some(data_type), Some(nullable), Some(ordinal)) = (
            row.get::<&str, _>(0),
            row.get::<&str, _>(1),
            row.get::<&str, _>(2),
            row.get::<i32, _>(3),
        ) {
            out.push(ColumnInfo {
                column: column.into(),
                data_type: data_type.into(),
                nullable: nullable.eq_ignore_ascii_case("YES"),
                ordinal,
            });
        }
    }
    out
}

/// Format a TDS decimal/numeric value with exact decimal placement.
///
/// tiberius stores decimals as an i128 plus a scale, and its own Display
/// mishandles negatives (-1999 at scale 2 renders as "-19.-99"), so the sign
/// is applied explicitly around the absolute digits. The result is returned
/// as a JSON string because f64 cannot hold every DECIMAL exactly.
pub(crate) fn format_numeric_value(n: Numeric) -> String {
    let scale = n.scale() as usize;
    if scale == 0 {
        return n.value().to_string();
    }
    let abs = n.value().unsigned_abs();
    let factor = 10u128.pow(scale as u32);
    let fraction = format!("{:0>width$}", abs % factor, width = scale);
    format!(
        "{}{}.{}",
        if n.value() < 0 { "-" } else { "" },
        abs / factor,
        fraction
    )
}

/// Hand-rolled hex encoding for binary/varbinary cells.
///
/// No extra dependency is pulled in for this single use; bytes render as
/// `0x`-prefixed uppercase hex so `0xAB` round-trips recognisably.
pub(crate) fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("0x");
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

/// Sentinel for cells whose TDS type has no JSON mapping.
///
/// Reached only when every typed probe failed: SQL NULL always satisfies at
/// least one probe with `Ok(None)`, so falling through here means the driver
/// genuinely cannot decode the value. Collapsing that to `null` would lie to
/// the LLM; the column type is named instead.
pub(crate) fn unsupported_sentinel(column_type: tiberius::ColumnType) -> Value {
    json!(format!("[UNSUPPORTED: {:?}]", column_type))
}

pub(crate) fn cell_to_json(row: &tiberius::Row, idx: usize) -> Value {
    // A probe `Err` means "wrong type, keep looking"; a decoded SQL NULL
    // (`Ok(None)`) is remembered so genuine NULLs stay JSON null while values
    // no probe understands fall through to the sentinel below.
    let mut saw_null = false;
    macro_rules! probe {
        ($ty:ty, $map:expr) => {
            match row.try_get::<$ty, _>(idx) {
                Ok(Some(v)) => return $map(v),
                Ok(None) => {
                    saw_null = true;
                }
                Err(_) => {}
            }
        };
    }

    // String first: most text-like columns land here.
    probe!(&str, |v: &str| json!(v));
    // Exact decimal/numeric display as string; never via f64.
    probe!(Numeric, |v: Numeric| json!(format_numeric_value(v)));
    // Integer types.
    probe!(i32, |v: i32| json!(v));
    probe!(i16, |v: i16| json!(v));
    probe!(i64, |v: i64| json!(v));
    probe!(u8, |v: u8| json!(v));
    // Floats. NOTE (MONEY limit): the TDS driver decodes MONEY/SMALLMONEY as
    // f64 (raw value / 1e4) before this function ever runs, so MONEY arrives
    // here indistinguishable from FLOAT and already subject to binary-float
    // rounding. Queries needing exact money arithmetic must CAST to DECIMAL
    // in SQL; this limit is documented rather than hidden.
    probe!(f32, |v: f32| json!(v));
    probe!(f64, |v: f64| json!(v));
    // Boolean.
    probe!(bool, |v: bool| json!(v));
    // Binary data as hand-rolled hex, never as lossy text.
    probe!(&[u8], |v: &[u8]| json!(bytes_to_hex(v)));
    // UUID as canonical string.
    probe!(uuid::Uuid, |v: uuid::Uuid| json!(v.to_string()));
    // Temporal types as strings (`NaiveTime` covers SQL TIME).
    probe!(NaiveDateTime, |v: NaiveDateTime| json!(v.to_string()));
    probe!(DateTime<Utc>, |v: DateTime<Utc>| json!(v.to_rfc3339()));
    probe!(NaiveDate, |v: NaiveDate| json!(v.to_string()));
    probe!(NaiveTime, |v: NaiveTime| json!(v.to_string()));
    if saw_null {
        return Value::Null;
    }
    let column_type = row
        .columns()
        .get(idx)
        .map(|c| c.column_type())
        .unwrap_or(tiberius::ColumnType::Null);
    unsupported_sentinel(column_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn numeric_formats_exact_with_two_places() {
        let n = tiberius::numeric::Numeric::new_with_scale(1999, 2);
        assert_eq!(format_numeric_value(n), "19.99");
    }

    #[test]
    fn numeric_formats_negative_exact() {
        // tiberius Display renders this as "-19.-99"; exact encoding must not.
        let n = tiberius::numeric::Numeric::new_with_scale(-1999, 2);
        assert_eq!(format_numeric_value(n), "-19.99");
    }

    #[test]
    fn numeric_formats_fraction_with_leading_zero() {
        assert_eq!(
            format_numeric_value(tiberius::numeric::Numeric::new_with_scale(5, 2)),
            "0.05"
        );
    }

    #[test]
    fn numeric_formats_scale_zero_as_integer() {
        assert_eq!(
            format_numeric_value(tiberius::numeric::Numeric::new_with_scale(100, 0)),
            "100"
        );
    }

    #[test]
    fn bytes_to_hex_single_byte() {
        assert_eq!(bytes_to_hex(&[0xAB]), "0xAB");
    }

    #[test]
    fn bytes_to_hex_preserves_leading_zeroes() {
        assert_eq!(bytes_to_hex(&[0x00, 0x0F]), "0x000F");
    }

    #[test]
    fn bytes_to_hex_empty_is_prefix_only() {
        assert_eq!(bytes_to_hex(&[]), "0x");
    }

    #[test]
    fn unsupported_sentinel_names_type_and_is_not_null() {
        let v = unsupported_sentinel(tiberius::ColumnType::Timen);
        assert_ne!(
            v,
            Value::Null,
            "unsupported types must never collapse to silent NULL"
        );
        let s = v.as_str().unwrap_or_default().to_owned();
        assert!(
            s.contains("[UNSUPPORTED"),
            "sentinel must carry [UNSUPPORTED marker, got: {s}"
        );
        assert!(
            s.contains("Timen"),
            "sentinel must name the column type, got: {s}"
        );
    }

    #[test]
    fn money_tds_cap_must_cast_to_decimal_for_exactness() {
        // TDS decodes MONEY/SMALLMONEY as f64 (raw/1e4) — binary-float rounding
        // applies before cell_to_json runs. Exact money arithmetic must CAST to
        // DECIMAL in SQL; DECIMAL arrives as Numeric and stays exact here.
        // This test pins the exact DECIMAL(19,4) path the CAST workaround relies on.
        let n = tiberius::numeric::Numeric::new_with_scale(199900, 4);
        assert_eq!(format_numeric_value(n), "19.9900");
    }
}

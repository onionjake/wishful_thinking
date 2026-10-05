//! Parsing and formatting of prices in the many shapes stores publish them.

/// Currencies that have no minor unit.
const ZERO_DECIMAL: &[&str] = &["JPY", "KRW", "VND", "CLP", "ISK", "HUF"];

pub fn minor_units(currency: &str) -> i64 {
    if ZERO_DECIMAL.contains(&currency.to_ascii_uppercase().as_str()) {
        1
    } else {
        100
    }
}

/// Map a currency symbol or prefix found in a price string to an ISO code.
pub fn currency_from_symbol(s: &str) -> Option<&'static str> {
    let s = s.trim();
    // Longer, more specific prefixes first.
    const TABLE: &[(&str, &str)] = &[
        ("CA$", "CAD"),
        ("C$", "CAD"),
        ("A$", "AUD"),
        ("AU$", "AUD"),
        ("NZ$", "NZD"),
        ("HK$", "HKD"),
        ("US$", "USD"),
        ("R$", "BRL"),
        ("MX$", "MXN"),
        ("$", "USD"),
        ("€", "EUR"),
        ("£", "GBP"),
        ("¥", "JPY"),
        ("₹", "INR"),
        ("₩", "KRW"),
        ("kr", "SEK"),
        ("zł", "PLN"),
        ("CHF", "CHF"),
    ];
    for (sym, code) in TABLE {
        if s.contains(sym) {
            return Some(code);
        }
    }
    // An ISO code written out ("USD 19.99", "19.99 EUR").
    for word in s.split(|c: char| !c.is_ascii_alphabetic()) {
        if word.len() == 3 && word.chars().all(|c| c.is_ascii_uppercase()) && is_known_code(word) {
            return Some(known_code(word));
        }
    }
    None
}

const CODES: &[&str] = &[
    "USD", "EUR", "GBP", "CAD", "AUD", "NZD", "JPY", "CHF", "SEK", "NOK", "DKK", "PLN", "INR",
    "BRL", "MXN", "HKD", "SGD", "KRW", "CNY", "ZAR",
];

fn is_known_code(code: &str) -> bool {
    CODES.contains(&code)
}

fn known_code(code: &str) -> &'static str {
    CODES.iter().find(|c| **c == code).copied().unwrap_or("USD")
}

/// Normalise a user- or site-provided currency code. Unknown values fall back to `None`.
pub fn normalize_currency(code: &str) -> Option<String> {
    let up = code.trim().to_ascii_uppercase();
    if up.len() == 3 && up.chars().all(|c| c.is_ascii_alphabetic()) {
        Some(up)
    } else {
        currency_from_symbol(code).map(str::to_string)
    }
}

/// Parse the numeric portion of a price string into an amount in minor units (cents).
///
/// Handles `1,299.99`, `1.299,99`, `1 299,99`, `19`, `19.9`, and strings with symbols or words
/// around them. The first number-like run in the string is used.
pub fn parse_amount(raw: &str, currency: &str) -> Option<i64> {
    // Take the first run that starts with a digit and contains digits, separators and spaces.
    let chars: Vec<char> = raw.chars().collect();
    let start = chars.iter().position(|c| c.is_ascii_digit())?;
    let mut end = start;
    while end < chars.len() {
        let c = chars[end];
        let sep_followed_by_digit = matches!(c, ',' | '.' | ' ' | '\u{a0}' | '\u{202f}' | '\'')
            && chars.get(end + 1).is_some_and(|n| n.is_ascii_digit());
        if c.is_ascii_digit() || sep_followed_by_digit {
            end += 1;
        } else {
            break;
        }
    }
    let run: String = chars[start..end]
        .iter()
        .filter(|c| !matches!(c, ' ' | '\u{a0}' | '\u{202f}' | '\''))
        .collect();

    let last_dot = run.rfind('.');
    let last_comma = run.rfind(',');
    let decimal_sep = match (last_dot, last_comma) {
        (Some(d), Some(c)) => Some(if d > c { '.' } else { ',' }),
        (Some(d), None) => {
            // "1.299" is ambiguous: treat as thousands when exactly three digits follow and
            // there are several dots, or the currency normally uses comma decimals.
            let after = run.len() - d - 1;
            if run.matches('.').count() > 1 || (after == 3 && uses_comma_decimal(currency)) {
                None
            } else {
                Some('.')
            }
        }
        (None, Some(c)) => {
            let after = run.len() - c - 1;
            if run.matches(',').count() == 1 && after != 3 {
                Some(',')
            } else {
                None
            }
        }
        (None, None) => None,
    };

    let (int_part, frac_part) = match decimal_sep {
        Some(sep) => {
            let idx = run.rfind(sep).unwrap();
            (&run[..idx], &run[idx + 1..])
        }
        None => (run.as_str(), ""),
    };
    let int_digits: String = int_part.chars().filter(|c| c.is_ascii_digit()).collect();
    let int_val: i64 = if int_digits.is_empty() {
        0
    } else {
        int_digits.parse().ok()?
    };
    let minor = minor_units(currency);
    let mut cents = int_val.checked_mul(minor)?;
    if minor == 100 && !frac_part.is_empty() {
        let mut frac: String = frac_part.chars().take(2).collect();
        while frac.len() < 2 {
            frac.push('0');
        }
        cents += frac.parse::<i64>().ok()?;
    }
    Some(cents)
}

fn uses_comma_decimal(currency: &str) -> bool {
    matches!(
        currency.to_ascii_uppercase().as_str(),
        "EUR" | "BRL" | "PLN" | "SEK" | "NOK" | "DKK" | "CHF"
    )
}

/// Parse a full price string (e.g. `"$1,299.99"`, `"24,95 €"`) into (minor units, currency).
pub fn parse_price(raw: &str, default_currency: Option<&str>) -> Option<(i64, String)> {
    let currency = currency_from_symbol(raw)
        .map(str::to_string)
        .or_else(|| default_currency.and_then(normalize_currency))
        .unwrap_or_else(|| "USD".to_string());
    let cents = parse_amount(raw, &currency)?;
    Some((cents, currency))
}

pub fn symbol(currency: &str) -> Option<&'static str> {
    Some(match currency {
        "USD" => "$",
        "EUR" => "€",
        "GBP" => "£",
        "JPY" => "¥",
        "CAD" => "CA$",
        "AUD" => "A$",
        "NZD" => "NZ$",
        "INR" => "₹",
        "KRW" => "₩",
        _ => return None,
    })
}

/// Format an amount in minor units for display, e.g. `$1,299.99`.
pub fn format(cents: i64, currency: &str) -> String {
    let minor = minor_units(currency);
    let whole = cents / minor;
    let frac = cents % minor;
    let mut grouped = String::new();
    let digits = whole.abs().to_string();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let number = if minor == 100 {
        format!("{grouped}.{frac:02}")
    } else {
        grouped
    };
    match symbol(currency) {
        Some(sym) => format!("{sym}{number}"),
        None => format!("{number} {currency}"),
    }
}

/// Format an amount in minor units as a plain decimal for an input field (`1299.99`).
pub fn to_input(cents: i64, currency: &str) -> String {
    let minor = minor_units(currency);
    if minor == 100 {
        format!("{}.{:02}", cents / 100, cents % 100)
    } else {
        cents.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_formats() {
        assert_eq!(parse_price("$1,299.99", None), Some((129999, "USD".into())));
        assert_eq!(parse_price("24,95 €", None), Some((2495, "EUR".into())));
        assert_eq!(parse_price("€1.299,00", None), Some((129900, "EUR".into())));
        assert_eq!(parse_price("£15", None), Some((1500, "GBP".into())));
        assert_eq!(parse_price("19.9", Some("USD")), Some((1990, "USD".into())));
        assert_eq!(parse_price("CA$ 45.00", None), Some((4500, "CAD".into())));
        assert_eq!(parse_price("¥3,980", None), Some((3980, "JPY".into())));
        assert_eq!(
            parse_price("1 299,95 kr", None),
            Some((129995, "SEK".into()))
        );
        assert_eq!(parse_price("USD 12.50", None), Some((1250, "USD".into())));
        assert_eq!(
            parse_price("Now only 1,299 dollars", Some("USD")),
            Some((129900, "USD".into()))
        );
        assert_eq!(parse_price("no price", None), None);
    }

    #[test]
    fn formats() {
        assert_eq!(format(129999, "USD"), "$1,299.99");
        assert_eq!(format(500, "EUR"), "€5.00");
        assert_eq!(format(3980, "JPY"), "¥3,980");
        assert_eq!(format(1234, "SEK"), "12.34 SEK");
        assert_eq!(to_input(1999, "USD"), "19.99");
    }
}

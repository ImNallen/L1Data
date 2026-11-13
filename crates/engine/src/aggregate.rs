use std::cmp::Ordering;

use crate::model::{CellAggregate, TypeCategory};

/// One selected column of full arena text. NULL is `None`. Empty string is `Some("")`.
pub(crate) struct ColumnText {
    pub display: String,
    pub category: TypeCategory,
    pub cells: Vec<Option<String>>,
}

/// Coefficient times `10^(-scale)`. Scale may be negative. Zero is never negative.
#[derive(Clone)]
struct Exact {
    negative: bool,
    coeff: Vec<u8>,
    scale: i64,
}

enum Parsed {
    Finite(Exact),
    Nan,
    PosInf,
    NegInf,
}

// A cell like `1e1000000000` is a few characters and a billion digits. Refuse it
// so a selection total cannot allocate that buffer. The column comes back as counts.
const MAX_DIGITS: usize = 1_048_576;

/// Counts, unless `columns` is one integer or numeric column whose display is not
/// `money` or `smallmoney`, every non-null cell parses, and at least one cell is a
/// finite number or a special. Two columns are counts even when both are summable.
pub(crate) fn aggregate(columns: &[ColumnText]) -> CellAggregate {
    let count = columns.iter().map(|c| c.cells.len() as u64).sum();
    let non_null = columns.iter().flat_map(|c| &c.cells).filter(|c| c.is_some()).count() as u64;
    let counts = CellAggregate::Counts { count, non_null };
    if columns.len() != 1 || !summable(&columns[0]) {
        return counts;
    }
    let mut values = Vec::new();
    for cell in &columns[0].cells {
        let Some(text) = cell else { continue };
        let Some(parsed) = parse_cell(text) else {
            return counts;
        };
        values.push(parsed);
    }
    if values.is_empty() {
        return counts;
    }
    let Some((sum, avg, min, max)) = summarize(&values, non_null) else {
        return counts;
    };
    CellAggregate::Numeric { count, non_null, sum, avg, min, max }
}

fn summable(col: &ColumnText) -> bool {
    matches!(col.category, TypeCategory::Integer | TypeCategory::Numeric)
        && col.display != "money"
        && col.display != "smallmoney"
}

fn parse_cell(text: &str) -> Option<Parsed> {
    if let Some(special) = parse_special(text) {
        return Some(special);
    }
    parse_finite(text).map(Parsed::Finite)
}

fn parse_special(text: &str) -> Option<Parsed> {
    let body = text.strip_prefix('+').unwrap_or(text);
    match body {
        "NaN" => Some(Parsed::Nan),
        "Infinity" | "inf" => Some(Parsed::PosInf),
        "-Infinity" | "-inf" => Some(Parsed::NegInf),
        _ => None,
    }
}

fn parse_finite(text: &str) -> Option<Exact> {
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut negative = false;
    if bytes.first() == Some(&b'+') {
        i = 1;
    } else if bytes.first() == Some(&b'-') {
        negative = true;
        i = 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let rest = &text[i..];
    let (mant, exp) = match rest.find(['e', 'E']) {
        Some(pos) => (&rest[..pos], parse_exponent(&rest[pos + 1..])?),
        None => (rest, 0),
    };
    let (int_part, frac_part) = split_mantissa(mant)?;
    let scale = i64::try_from(frac_part.len()).ok()?.checked_sub(exp)?;
    let mut digits = Vec::with_capacity(int_part.len() + frac_part.len());
    for b in int_part.bytes().chain(frac_part.bytes()) {
        if !b.is_ascii_digit() {
            return None;
        }
        digits.push(b - b'0');
    }
    let coeff = strip_leading_zeros(digits);
    let negative = negative && !is_zero(&coeff);
    if !within_budget(&coeff, scale) {
        return None;
    }
    Some(Exact { negative, coeff, scale })
}

fn split_mantissa(mant: &str) -> Option<(&str, &str)> {
    match mant.split_once('.') {
        None => {
            if mant.is_empty() || !mant.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some((mant, ""))
        }
        Some((int_part, frac)) => {
            if int_part.is_empty() || frac.is_empty() {
                return None;
            }
            if !int_part.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some((int_part, frac))
        }
    }
}

fn parse_exponent(text: &str) -> Option<i64> {
    if text.is_empty() {
        return None;
    }
    let (sign, digits) = match text.as_bytes()[0] {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => (1, text),
    };
    if digits.is_empty() {
        return None;
    }
    let mut n = 0i64;
    for b in digits.bytes() {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add(i64::from(b - b'0'))?;
    }
    if sign < 0 { n.checked_neg() } else { Some(n) }
}

fn summarize(values: &[Parsed], non_null: u64) -> Option<(String, String, String, String)> {
    let mut nan = false;
    let mut pos_inf = false;
    let mut neg_inf = false;
    let mut finites = Vec::new();
    for value in values {
        match value {
            Parsed::Nan => nan = true,
            Parsed::PosInf => pos_inf = true,
            Parsed::NegInf => neg_inf = true,
            Parsed::Finite(exact) => finites.push(exact.clone()),
        }
    }
    if nan {
        let nan = "NaN".to_owned();
        return Some((nan.clone(), nan.clone(), nan.clone(), nan));
    }
    let finite_sum = if finites.is_empty() { None } else { Some(sum_finites(&finites)?) };
    let (sum, avg) = if pos_inf && neg_inf {
        ("NaN".to_owned(), "NaN".to_owned())
    } else if pos_inf {
        ("Infinity".to_owned(), "Infinity".to_owned())
    } else if neg_inf {
        ("-Infinity".to_owned(), "-Infinity".to_owned())
    } else {
        let sum = finite_sum.as_ref()?;
        // Exponent text parses with a negative scale. Adding six there would
        // round through integer digits, so the average always keeps six
        // fractional digits.
        let avg_scale = sum.scale.max(0).checked_add(6)?;
        let avg = div_half_even(sum, non_null, avg_scale)?;
        (print_exact(sum), print_exact(&avg))
    };
    let min = if neg_inf {
        "-Infinity".to_owned()
    } else if let Some(least) = finites.iter().min_by(|a, b| cmp_exact(a, b)) {
        print_exact(least)
    } else if pos_inf {
        "Infinity".to_owned()
    } else {
        return None;
    };
    let max = if pos_inf {
        "Infinity".to_owned()
    } else if let Some(greatest) = finites.iter().max_by(|a, b| cmp_exact(a, b)) {
        print_exact(greatest)
    } else if neg_inf {
        "-Infinity".to_owned()
    } else {
        return None;
    };
    Some((sum, avg, min, max))
}

fn sum_finites(values: &[Exact]) -> Option<Exact> {
    let scale = values.iter().map(|v| v.scale).max()?;
    let mut acc = Exact { negative: false, coeff: vec![0], scale };
    for value in values {
        acc = add_exact(&acc, &align(value, scale)?)?;
    }
    Some(acc)
}

fn align(exact: &Exact, scale: i64) -> Option<Exact> {
    if scale < exact.scale {
        return None;
    }
    let delta = usize::try_from(scale - exact.scale).ok()?;
    let mut coeff = exact.coeff.clone();
    if !is_zero(&coeff) {
        if coeff.len().saturating_add(delta) > MAX_DIGITS {
            return None;
        }
        coeff.extend(std::iter::repeat(0).take(delta));
    }
    Some(Exact { negative: exact.negative, coeff, scale })
}

fn add_exact(left: &Exact, right: &Exact) -> Option<Exact> {
    let scale = left.scale;
    if left.negative == right.negative {
        let coeff = add_mag(&left.coeff, &right.coeff);
        if coeff.len() > MAX_DIGITS {
            return None;
        }
        return Some(Exact { negative: left.negative && !is_zero(&coeff), coeff, scale });
    }
    let (coeff, negative) = match cmp_mag(&left.coeff, &right.coeff) {
        Ordering::Equal => (vec![0], false),
        Ordering::Greater => (sub_mag(&left.coeff, &right.coeff), left.negative),
        Ordering::Less => (sub_mag(&right.coeff, &left.coeff), right.negative),
    };
    Some(Exact { negative: negative && !is_zero(&coeff), coeff, scale })
}

fn div_half_even(sum: &Exact, divisor: u64, target_scale: i64) -> Option<Exact> {
    if divisor == 0 {
        return None;
    }
    let extra = usize::try_from(target_scale.checked_sub(sum.scale)?).ok()?;
    let mut coeff = sum.coeff.clone();
    if !is_zero(&coeff) {
        if coeff.len().saturating_add(extra) > MAX_DIGITS {
            return None;
        }
        coeff.extend(std::iter::repeat(0).take(extra));
    }
    let coeff = div_mag(&coeff, divisor);
    if coeff.len() > MAX_DIGITS {
        return None;
    }
    Some(Exact { negative: sum.negative && !is_zero(&coeff), coeff, scale: target_scale })
}

fn div_mag(coeff: &[u8], divisor: u64) -> Vec<u8> {
    if is_zero(coeff) {
        return vec![0];
    }
    let div = u128::from(divisor);
    let mut rem = 0u128;
    let mut quot = Vec::with_capacity(coeff.len());
    for &digit in coeff {
        rem = rem * 10 + u128::from(digit);
        let next = (rem / div) as u8;
        rem %= div;
        if !quot.is_empty() || next != 0 {
            quot.push(next);
        }
    }
    if quot.is_empty() {
        quot.push(0);
    }
    // A tie is an exact half. Half-even keeps an even last digit.
    let round_away = match (rem * 2).cmp(&div) {
        Ordering::Less => false,
        Ordering::Greater => true,
        Ordering::Equal => quot.last().copied().unwrap_or(0) % 2 == 1,
    };
    if round_away {
        add_one(&mut quot);
    }
    quot
}

fn add_one(quot: &mut Vec<u8>) {
    let mut i = quot.len();
    while i > 0 {
        i -= 1;
        if quot[i] < 9 {
            quot[i] += 1;
            return;
        }
        quot[i] = 0;
    }
    quot.insert(0, 1);
}

fn cmp_exact(left: &Exact, right: &Exact) -> Ordering {
    match (left.negative, right.negative) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => cmp_abs(left, right),
        (true, true) => cmp_abs(right, left),
    }
}

fn cmp_abs(left: &Exact, right: &Exact) -> Ordering {
    if is_zero(&left.coeff) && is_zero(&right.coeff) {
        return Ordering::Equal;
    }
    if is_zero(&left.coeff) {
        return Ordering::Less;
    }
    if is_zero(&right.coeff) {
        return Ordering::Greater;
    }
    let (left_extra, right_extra) = if left.scale >= right.scale {
        (0usize, (left.scale - right.scale) as usize)
    } else {
        ((right.scale - left.scale) as usize, 0)
    };
    let left_len = left.coeff.len() + left_extra;
    let right_len = right.coeff.len() + right_extra;
    match left_len.cmp(&right_len) {
        Ordering::Equal => {}
        other => return other,
    }
    for i in 0..left_len {
        let l = if i < left.coeff.len() { left.coeff[i] } else { 0 };
        let r = if i < right.coeff.len() { right.coeff[i] } else { 0 };
        match l.cmp(&r) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    Ordering::Equal
}

fn cmp_mag(left: &[u8], right: &[u8]) -> Ordering {
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

fn add_mag(left: &[u8], right: &[u8]) -> Vec<u8> {
    let n = left.len().max(right.len());
    let mut out = vec![0u8; n + 1];
    let mut carry = 0u8;
    for i in 0..n {
        let sum = digit_from_end(left, i) + digit_from_end(right, i) + carry;
        out[n - i] = sum % 10;
        carry = sum / 10;
    }
    out[0] = carry;
    strip_leading_zeros(out)
}

fn sub_mag(left: &[u8], right: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; left.len()];
    let mut borrow = 0i16;
    for i in 0..left.len() {
        let mut value = i16::from(left[left.len() - 1 - i]) - i16::from(digit_from_end(right, i)) - borrow;
        if value < 0 {
            value += 10;
            borrow = 1;
        } else {
            borrow = 0;
        }
        out[left.len() - 1 - i] = value as u8;
    }
    strip_leading_zeros(out)
}

fn digit_from_end(digits: &[u8], index: usize) -> u8 {
    if index < digits.len() { digits[digits.len() - 1 - index] } else { 0 }
}

fn strip_leading_zeros(digits: Vec<u8>) -> Vec<u8> {
    let start = digits.iter().position(|&d| d != 0).unwrap_or(digits.len().saturating_sub(1));
    if start == 0 { digits } else { digits[start..].to_vec() }
}

fn is_zero(digits: &[u8]) -> bool {
    digits.iter().all(|&d| d == 0)
}

fn within_budget(coeff: &[u8], scale: i64) -> bool {
    let tail = if scale < 0 {
        match usize::try_from(scale.unsigned_abs()) {
            Ok(n) => n,
            Err(_) => return false,
        }
    } else if scale > MAX_DIGITS as i64 {
        return false;
    } else {
        0
    };
    coeff.len().saturating_add(tail) <= MAX_DIGITS
}

fn print_exact(exact: &Exact) -> String {
    let mut out = String::new();
    if exact.negative {
        out.push('-');
    }
    let digits = &exact.coeff;
    if exact.scale > 0 {
        let scale = exact.scale as usize;
        if digits.len() <= scale {
            out.push('0');
            out.push('.');
            for _ in 0..(scale - digits.len()) {
                out.push('0');
            }
            push_digits(&mut out, digits);
        } else {
            let split = digits.len() - scale;
            push_digits(&mut out, &digits[..split]);
            out.push('.');
            push_digits(&mut out, &digits[split..]);
        }
    } else {
        push_digits(&mut out, digits);
        if exact.scale < 0 {
            for _ in 0..(-exact.scale) as usize {
                out.push('0');
            }
        }
    }
    out
}

fn push_digits(out: &mut String, digits: &[u8]) {
    for &digit in digits {
        out.push(char::from(b'0' + digit));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(display: &str, category: TypeCategory, cells: &[Option<&str>]) -> ColumnText {
        ColumnText { display: display.into(), category, cells: cells.iter().map(|c| c.map(str::to_owned)).collect() }
    }

    fn numeric(cells: &[Option<&str>]) -> ColumnText {
        column("numeric", TypeCategory::Numeric, cells)
    }

    fn nums(cells: &[&str]) -> ColumnText {
        numeric(&cells.iter().map(|c| Some(*c)).collect::<Vec<_>>())
    }

    fn sum_of(agg: CellAggregate) -> String {
        match agg {
            CellAggregate::Numeric { sum, .. } => sum,
            CellAggregate::Counts { .. } => panic!("counts"),
        }
    }

    fn avg_of(agg: CellAggregate) -> String {
        match agg {
            CellAggregate::Numeric { avg, .. } => avg,
            CellAggregate::Counts { .. } => panic!("counts"),
        }
    }

    fn quad(agg: CellAggregate) -> (String, String, String, String) {
        match agg {
            CellAggregate::Numeric { sum, avg, min, max, .. } => (sum, avg, min, max),
            CellAggregate::Counts { .. } => panic!("counts"),
        }
    }

    #[test]
    fn tenth_plus_two_tenths_keeps_one_decimal_and_pads_the_average() {
        assert_eq!(
            aggregate(&[nums(&["0.1", "0.2"])]),
            CellAggregate::Numeric {
                count: 2,
                non_null: 2,
                sum: "0.3".into(),
                avg: "0.1500000".into(),
                min: "0.1".into(),
                max: "0.2".into()
            }
        );
        assert_eq!(
            aggregate(&[nums(&["0.10", "0.20"])]),
            CellAggregate::Numeric {
                count: 2,
                non_null: 2,
                sum: "0.30".into(),
                avg: "0.15000000".into(),
                min: "0.10".into(),
                max: "0.20".into()
            }
        );
    }

    #[test]
    fn sum_past_two_to_the_sixty_three_keeps_every_digit() {
        assert_eq!(sum_of(aggregate(&[nums(&["9223372036854775808", "9223372036854775808"])])), "18446744073709551616");
    }

    #[test]
    fn negative_scale_and_exponents_print_without_an_exponent() {
        assert_eq!(sum_of(aggregate(&[nums(&["12e2", "3e2"])])), "1500");
        assert_eq!(sum_of(aggregate(&[nums(&["1.5e+2", "1.5e-1"])])), "150.15");
    }

    #[test]
    fn negative_scale_average_keeps_six_fractional_digits() {
        let exponent = nums(&["1e+20", "1e+20", "2e+20"]);
        let plain = nums(&["100000000000000000000", "100000000000000000000", "200000000000000000000"]);
        assert_eq!(sum_of(aggregate(&[exponent])), "400000000000000000000");
        assert_eq!(avg_of(aggregate(&[nums(&["1e+20", "1e+20", "2e+20"])])), "133333333333333333333.333333");
        assert_eq!(avg_of(aggregate(&[plain])), "133333333333333333333.333333");
        assert_eq!(avg_of(aggregate(&[nums(&["1e+20"])])), "100000000000000000000.000000");
    }

    #[test]
    fn half_even_average_uses_non_null_and_six_extra_digits() {
        let mut mostly_zero = vec!["0"; 128];
        mostly_zero[0] = "1";
        assert_eq!(avg_of(aggregate(&[nums(&mostly_zero)])), "0.007812");
        mostly_zero[0] = "3";
        assert_eq!(avg_of(aggregate(&[nums(&mostly_zero)])), "0.023438");
        assert_eq!(avg_of(aggregate(&[nums(&["1", "1", "0"])])), "0.666667");
    }

    #[test]
    fn null_increments_only_count_and_an_empty_string_is_non_null() {
        assert_eq!(
            aggregate(&[numeric(&[Some("1"), None])]),
            CellAggregate::Numeric {
                count: 2,
                non_null: 1,
                sum: "1".into(),
                avg: "1.000000".into(),
                min: "1".into(),
                max: "1".into()
            }
        );
        assert_eq!(aggregate(&[numeric(&[None, None])]), CellAggregate::Counts { count: 2, non_null: 0 });
        assert_eq!(aggregate(&[numeric(&[Some("")])]), CellAggregate::Counts { count: 1, non_null: 1 });
        assert_eq!(aggregate(&[numeric(&[Some("1"), Some("")])]), CellAggregate::Counts { count: 2, non_null: 2 });
    }

    #[test]
    fn specials_poison_the_sum_and_order_below_and_above_finites() {
        assert_eq!(quad(aggregate(&[nums(&["NaN", "1"])])), ("NaN".into(), "NaN".into(), "NaN".into(), "NaN".into()));
        assert_eq!(
            quad(aggregate(&[nums(&["+Infinity", "inf", "4"])])),
            ("Infinity".into(), "Infinity".into(), "4".into(), "Infinity".into())
        );
        assert_eq!(
            quad(aggregate(&[nums(&["-inf", "-Infinity", "4"])])),
            ("-Infinity".into(), "-Infinity".into(), "-Infinity".into(), "4".into())
        );
        assert_eq!(
            quad(aggregate(&[nums(&["Infinity", "-inf", "1"])])),
            ("NaN".into(), "NaN".into(), "-Infinity".into(), "Infinity".into())
        );
        assert_eq!(quad(aggregate(&[nums(&["NaN"])])), ("NaN".into(), "NaN".into(), "NaN".into(), "NaN".into()));
        assert_eq!(
            aggregate(&[nums(&["-0", "0.0"])]),
            CellAggregate::Numeric {
                count: 2,
                non_null: 2,
                sum: "0.0".into(),
                avg: "0.0000000".into(),
                min: "0".into(),
                max: "0.0".into()
            }
        );
    }

    #[test]
    fn money_text_and_a_second_column_are_counts() {
        assert_eq!(
            aggregate(&[column("money", TypeCategory::Numeric, &[Some("$1,234.00")])]),
            CellAggregate::Counts { count: 1, non_null: 1 }
        );
        assert_eq!(
            aggregate(&[column("smallmoney", TypeCategory::Numeric, &[Some("12.3456")])]),
            CellAggregate::Counts { count: 1, non_null: 1 }
        );
        assert_eq!(
            aggregate(&[
                column("bigint", TypeCategory::Integer, &[Some("1")]),
                column("numeric", TypeCategory::Numeric, &[Some("2.00")])
            ]),
            CellAggregate::Counts { count: 2, non_null: 2 }
        );
        assert_eq!(
            aggregate(&[
                column("numeric", TypeCategory::Numeric, &[Some("2.00")]),
                column("text", TypeCategory::Text, &[Some("paid")])
            ]),
            CellAggregate::Counts { count: 2, non_null: 2 }
        );
        assert_eq!(
            aggregate(&[column("text", TypeCategory::Text, &[Some("1"), Some("2")])]),
            CellAggregate::Counts { count: 2, non_null: 2 }
        );
        assert_eq!(aggregate(&[numeric(&[Some("nope")])]), CellAggregate::Counts { count: 1, non_null: 1 });
    }
}

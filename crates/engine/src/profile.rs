use std::collections::HashMap;

use crate::aggregate::{ColumnText, aggregate};
use crate::model::{CellAggregate, ColumnProfile, HistogramBin, ProfileSummary, ValueCount};
use crate::results::CELL_CHAR_LIMIT;

const BINS: usize = 12;
const TOP: usize = 5;

pub(crate) fn profile(column: &ColumnText) -> ColumnProfile {
    let mut nulls = 0u64;
    // Each distinct text's count and the row it first appears in, which breaks ties in the top values.
    let mut seen: HashMap<&str, (u64, usize)> = HashMap::new();
    for (row, cell) in column.cells.iter().enumerate() {
        match cell {
            None => nulls += 1,
            Some(text) => seen.entry(text.as_str()).or_insert((0, row)).0 += 1,
        }
    }
    let summary = match aggregate(std::slice::from_ref(column)) {
        CellAggregate::Numeric { min, avg, max, .. } => {
            ProfileSummary::Numeric { min, mean: avg, max, histogram: histogram(column) }
        }
        CellAggregate::Counts { .. } => ProfileSummary::Values { top: top(&seen) },
    };
    ColumnProfile { rows: column.cells.len() as u64, nulls, distinct: seen.len() as u64, summary }
}

fn histogram(column: &ColumnText) -> Vec<HistogramBin> {
    let values: Vec<f64> =
        column.cells.iter().flatten().filter_map(|text| text.parse::<f64>().ok()).filter(|v| v.is_finite()).collect();
    let Some(min) = values.iter().copied().reduce(f64::min) else { return Vec::new() };
    let max = values.iter().copied().fold(min, f64::max);
    if min == max {
        return vec![HistogramBin { lo: min, hi: max, count: values.len() as u64 }];
    }
    // Interpolated so the edges stay finite even when `max - min` overflows.
    let edges: Vec<f64> = (0..=BINS)
        .map(|i| {
            let t = i as f64 / BINS as f64;
            min * (1.0 - t) + max * t
        })
        .collect();
    let mut counts = [0u64; BINS];
    for v in values {
        counts[edges[1..BINS].partition_point(|&edge| edge <= v)] += 1;
    }
    counts.iter().enumerate().map(|(i, &count)| HistogramBin { lo: edges[i], hi: edges[i + 1], count }).collect()
}

fn top(seen: &HashMap<&str, (u64, usize)>) -> Vec<ValueCount> {
    let mut ranked: Vec<(&str, u64, usize)> =
        seen.iter().map(|(text, &(count, first))| (*text, count, first)).collect();
    ranked.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
    ranked
        .into_iter()
        .take(TOP)
        .map(|(text, count, _)| {
            let keep = text.char_indices().nth(CELL_CHAR_LIMIT).map_or(text.len(), |(b, _)| b);
            ValueCount { value: text[..keep].to_owned(), count }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TypeCategory;

    fn column(display: &str, category: TypeCategory, cells: &[Option<&str>]) -> ColumnText {
        ColumnText { display: display.into(), category, cells: cells.iter().map(|c| c.map(str::to_owned)).collect() }
    }

    fn numeric(cells: &[Option<&str>]) -> ColumnText {
        column("numeric", TypeCategory::Numeric, cells)
    }

    fn text(cells: &[Option<&str>]) -> ColumnText {
        column("text", TypeCategory::Text, cells)
    }

    fn top_of(profile: &ColumnProfile) -> Vec<(&str, u64)> {
        match &profile.summary {
            ProfileSummary::Values { top } => top.iter().map(|v| (v.value.as_str(), v.count)).collect(),
            ProfileSummary::Numeric { .. } => panic!("numeric"),
        }
    }

    #[test]
    fn counts_rows_nulls_and_distinct_texts_with_empty_as_a_value() {
        let p = profile(&text(&[Some("a"), None, Some(""), Some("a"), None, Some("b")]));
        assert_eq!((p.rows, p.nulls, p.distinct), (6, 2, 3));
    }

    #[test]
    fn numeric_column_has_exact_stats_and_bins_that_hold_every_finite_value() {
        let cells: Vec<String> =
            std::iter::once(0).chain((1..24).step_by(2)).chain([24]).map(|i| format!("{i}.00")).collect();
        let mut column: Vec<Option<&str>> = cells.iter().map(|c| Some(c.as_str())).collect();
        column.push(None);
        let p = profile(&numeric(&column));
        assert_eq!((p.rows, p.nulls, p.distinct), (15, 1, 14));
        let ProfileSummary::Numeric { min, mean, max, histogram } = p.summary else { panic!("values") };
        assert_eq!((min.as_str(), mean.as_str(), max.as_str()), ("0.00", "12.00000000", "24.00"));
        assert_eq!(histogram.iter().map(|b| b.count).collect::<Vec<_>>(), vec![2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2]);
        assert_eq!((histogram[0].lo, histogram[0].hi, histogram[11].hi), (0.0, 2.0, 24.0));
        for pair in histogram.windows(2) {
            assert_eq!(pair[0].hi, pair[1].lo);
        }
    }

    #[test]
    fn the_maximum_lands_in_the_last_bin() {
        let ProfileSummary::Numeric { histogram, .. } = profile(&numeric(&[Some("0"), Some("12")])).summary else {
            panic!("values")
        };
        assert_eq!(histogram[0].count, 1);
        assert_eq!(histogram[11].count, 1);
    }

    #[test]
    fn non_finite_values_are_left_out_of_the_bins() {
        let ProfileSummary::Numeric { min, max, histogram, .. } =
            profile(&numeric(&[Some("1"), Some("Infinity"), Some("3")])).summary
        else {
            panic!("values")
        };
        assert_eq!((min.as_str(), max.as_str()), ("1", "Infinity"));
        assert_eq!((histogram[0].lo, histogram[11].hi), (1.0, 3.0));
        assert_eq!(histogram.iter().map(|b| b.count).sum::<u64>(), 2);
        let ProfileSummary::Numeric { histogram, .. } = profile(&numeric(&[Some("NaN")])).summary else {
            panic!("values")
        };
        assert!(histogram.is_empty());
    }

    #[test]
    fn edges_stay_finite_when_the_range_overflows() {
        let ProfileSummary::Numeric { histogram, .. } = profile(&numeric(&[Some("-1e308"), Some("1e308")])).summary
        else {
            panic!("values")
        };
        assert!(histogram.iter().all(|b| b.lo.is_finite() && b.hi.is_finite()));
        assert_eq!(histogram.iter().map(|b| b.count).sum::<u64>(), 2);
    }

    #[test]
    fn one_repeated_value_is_one_bin() {
        let p = profile(&column("integer", TypeCategory::Integer, &[Some("7"), Some("7"), None]));
        assert_eq!(p.distinct, 1);
        let ProfileSummary::Numeric { min, mean, max, histogram } = p.summary else { panic!("values") };
        assert_eq!((min.as_str(), mean.as_str(), max.as_str()), ("7", "7.000000", "7"));
        assert_eq!(histogram, vec![HistogramBin { lo: 7.0, hi: 7.0, count: 2 }]);
    }

    #[test]
    fn money_and_unparsable_numbers_fall_back_to_top_values() {
        let p = profile(&column("money", TypeCategory::Numeric, &[Some("$1.00"), Some("$1.00"), Some("$2.00")]));
        assert_eq!(top_of(&p), vec![("$1.00", 2), ("$2.00", 1)]);
        let p = profile(&numeric(&[Some("1"), Some("oops")]));
        assert_eq!(top_of(&p), vec![("1", 1), ("oops", 1)]);
    }

    #[test]
    fn top_values_rank_by_count_then_first_appearance_and_skip_null() {
        let p = profile(&text(&[
            Some("e"),
            None,
            Some("d"),
            Some("c"),
            Some("c"),
            Some("b"),
            Some("a"),
            Some("f"),
            Some("a"),
            None,
            None,
            Some("g"),
        ]));
        assert_eq!(top_of(&p), vec![("c", 2), ("a", 2), ("e", 1), ("d", 1), ("b", 1)]);
    }

    #[test]
    fn a_long_value_is_cut_like_a_page_cell() {
        let long = "é".repeat(CELL_CHAR_LIMIT + 3);
        let p = profile(&text(&[Some(long.as_str())]));
        assert_eq!(top_of(&p)[0].0.chars().count(), CELL_CHAR_LIMIT);
    }

    #[test]
    fn an_all_null_column_has_no_values() {
        let p = profile(&numeric(&[None, None, None]));
        assert_eq!((p.rows, p.nulls, p.distinct), (3, 3, 0));
        assert_eq!(top_of(&p), vec![]);
        let p = profile(&text(&[]));
        assert_eq!((p.rows, p.nulls, p.distinct), (0, 0, 0));
        assert_eq!(top_of(&p), vec![]);
    }
}

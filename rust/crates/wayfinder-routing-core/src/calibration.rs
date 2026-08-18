//! Pure threshold calibration algorithms.
//!
//! Calibration consumes scores and labels only. Dataset parsing, file access,
//! and configuration emission belong to outer crates so this module remains
//! deterministic and platform-neutral.

use thiserror::Error;

use crate::python_round;

const OBJECTIVE_EPSILON: f64 = 1e-12;

/// One scored prompt used by binary threshold calibration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThresholdSample {
    /// Rounded Wayfinder score in the inclusive `0.0..=1.0` domain.
    pub score: f64,
    /// Whether the prompt requires the high-quality arm.
    pub requires_high: bool,
}

/// Result of minimizing routed money plus the missed-high penalty.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MinCostCalibration {
    /// Inclusive score boundary for the high-quality arm.
    pub threshold: f64,
    /// Fraction of prompts whose selected arm matches the label.
    pub accuracy: f64,
    /// Fraction of high-labelled prompts routed to the high-quality arm.
    pub quality_recovered: f64,
    /// Fractional saving against routing every prompt to the high-cost arm.
    pub cost_savings: f64,
    /// Mean configured arm cost per prompt.
    pub expected_money_cost: f64,
    /// Mean money cost plus `quality_penalty` for each missed-high prompt.
    pub expected_loss: f64,
}

/// Invalid input to deterministic threshold calibration.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum CalibrationError {
    /// No samples were supplied.
    #[error("calibration needs at least one sample")]
    EmptyDataset,
    /// Both label classes must be represented.
    #[error("calibration needs at least one low-labelled and one high-labelled sample")]
    MissingLabelClass,
    /// A score was not finite or inside the scorer domain.
    #[error("sample {index} has score {score}; scores must be finite and between 0.0 and 1.0")]
    InvalidScore {
        /// Zero-based sample index.
        index: usize,
        /// Invalid score.
        score: f64,
    },
    /// An arm cost or quality penalty was invalid.
    #[error("{name} must be a finite non-negative number, got {value}")]
    InvalidCost {
        /// Input name.
        name: &'static str,
        /// Invalid value.
        value: f64,
    },
    /// The two arms do not define a cheap/high-cost ordering.
    #[error("high_cost must be greater than low_cost")]
    UnorderedCosts,
}

/// Select the deterministic threshold that minimizes expected economic loss.
///
/// A prompt routed at or above `threshold` pays `high_cost`. A prompt below it
/// pays `low_cost`, plus `quality_penalty` when its label requires the high arm.
/// Candidate cuts are `0.0`, `1.0`, and every observed score rounded to four
/// decimals. Exact objective ties choose the upper median cut.
pub fn calibrate_min_cost(
    samples: &[ThresholdSample],
    low_cost: f64,
    high_cost: f64,
    quality_penalty: f64,
) -> Result<MinCostCalibration, CalibrationError> {
    validate_inputs(samples, low_cost, high_cost, quality_penalty)?;

    let mut candidates = Vec::with_capacity(samples.len().saturating_add(2));
    candidates.push(0.0);
    candidates.push(1.0);
    candidates.extend(samples.iter().map(|sample| python_round(sample.score, 4)));
    candidates.sort_by(f64::total_cmp);
    candidates.dedup_by(|left, right| left.total_cmp(right).is_eq());

    let mut best_loss = f64::INFINITY;
    let mut best_cuts = Vec::new();
    for candidate in candidates {
        let metrics = metrics_at(samples, candidate, low_cost, high_cost, quality_penalty);
        if metrics.expected_loss < best_loss - OBJECTIVE_EPSILON {
            best_loss = metrics.expected_loss;
            best_cuts.clear();
            best_cuts.push(candidate);
        } else if (metrics.expected_loss - best_loss).abs() <= OBJECTIVE_EPSILON {
            best_cuts.push(candidate);
        }
    }

    let chosen = best_cuts.get(best_cuts.len() / 2).copied().unwrap_or(0.0);
    Ok(metrics_at(
        samples,
        chosen,
        low_cost,
        high_cost,
        quality_penalty,
    ))
}

fn validate_inputs(
    samples: &[ThresholdSample],
    low_cost: f64,
    high_cost: f64,
    quality_penalty: f64,
) -> Result<(), CalibrationError> {
    if samples.is_empty() {
        return Err(CalibrationError::EmptyDataset);
    }
    for (index, sample) in samples.iter().enumerate() {
        if !sample.score.is_finite() || !(0.0..=1.0).contains(&sample.score) {
            return Err(CalibrationError::InvalidScore {
                index,
                score: sample.score,
            });
        }
    }
    if !samples.iter().any(|sample| sample.requires_high)
        || !samples.iter().any(|sample| !sample.requires_high)
    {
        return Err(CalibrationError::MissingLabelClass);
    }
    validate_cost("low_cost", low_cost)?;
    validate_cost("high_cost", high_cost)?;
    validate_cost("quality_penalty", quality_penalty)?;
    if high_cost <= low_cost {
        return Err(CalibrationError::UnorderedCosts);
    }
    Ok(())
}

fn validate_cost(name: &'static str, value: f64) -> Result<(), CalibrationError> {
    if !value.is_finite() || value < 0.0 {
        return Err(CalibrationError::InvalidCost { name, value });
    }
    Ok(())
}

fn metrics_at(
    samples: &[ThresholdSample],
    threshold: f64,
    low_cost: f64,
    high_cost: f64,
    quality_penalty: f64,
) -> MinCostCalibration {
    let total = samples.len() as f64;
    let high_labels = samples.iter().filter(|sample| sample.requires_high).count() as f64;
    let mut correct = 0_usize;
    let mut recovered = 0_usize;
    let mut missed_high = 0_usize;
    let mut money = 0.0;

    for sample in samples {
        let routed_high = sample.score >= threshold;
        money += if routed_high { high_cost } else { low_cost };
        if routed_high == sample.requires_high {
            correct = correct.saturating_add(1);
        }
        if sample.requires_high {
            if routed_high {
                recovered = recovered.saturating_add(1);
            } else {
                missed_high = missed_high.saturating_add(1);
            }
        }
    }

    let expected_money_cost = money / total;
    let missed_high_rate = missed_high as f64 / total;
    MinCostCalibration {
        threshold,
        accuracy: correct as f64 / total,
        quality_recovered: recovered as f64 / high_labels,
        cost_savings: (high_cost - expected_money_cost) / high_cost,
        expected_money_cost,
        expected_loss: expected_money_cost + quality_penalty * missed_high_rate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn separating_samples() -> Vec<ThresholdSample> {
        (0_u32..100)
            .map(|index| {
                let score = f64::from(index) / 100.0;
                let requires_high = if index % 5 == 0 {
                    score < 0.2
                } else {
                    score >= 0.4
                };
                ThresholdSample {
                    score,
                    requires_high,
                }
            })
            .collect()
    }

    #[test]
    fn prices_move_the_selected_cut() -> Result<(), CalibrationError> {
        let samples = separating_samples();
        let narrow_gap = calibrate_min_cost(&samples, 0.9, 1.0, 1.0)?;
        let wide_gap = calibrate_min_cost(&samples, 0.05, 1.0, 1.0)?;
        assert!(wide_gap.threshold > narrow_gap.threshold);
        Ok(())
    }

    #[test]
    fn larger_penalty_recovers_more_high_labels() -> Result<(), CalibrationError> {
        let samples = separating_samples();
        let lax = calibrate_min_cost(&samples, 0.2, 1.0, 0.5)?;
        let strict = calibrate_min_cost(&samples, 0.2, 1.0, 50.0)?;
        assert!(strict.threshold <= lax.threshold);
        assert!(strict.quality_recovered >= lax.quality_recovered);
        Ok(())
    }

    #[test]
    fn zero_penalty_selects_the_score_ceiling() -> Result<(), CalibrationError> {
        let result = calibrate_min_cost(&separating_samples(), 0.05, 1.0, 0.0)?;
        assert_eq!(result.threshold, 1.0);
        assert_eq!(result.quality_recovered, 0.0);
        Ok(())
    }

    #[test]
    fn ties_choose_the_upper_median_candidate() -> Result<(), CalibrationError> {
        let samples = [
            ThresholdSample {
                score: 0.2,
                requires_high: false,
            },
            ThresholdSample {
                score: 0.8,
                requires_high: true,
            },
        ];
        let first = calibrate_min_cost(&samples, 0.0, 1.0, 1.0)?;
        let second = calibrate_min_cost(&samples, 0.0, 1.0, 1.0)?;
        assert_eq!(first, second);
        assert_eq!(first.threshold, 1.0);
        Ok(())
    }

    #[test]
    fn invalid_inputs_fail_closed() {
        let samples = separating_samples();
        assert_eq!(
            calibrate_min_cost(&samples, 0.2, 1.0, -1.0),
            Err(CalibrationError::InvalidCost {
                name: "quality_penalty",
                value: -1.0,
            })
        );
        assert_eq!(
            calibrate_min_cost(&samples, 1.0, 1.0, 1.0),
            Err(CalibrationError::UnorderedCosts)
        );
        assert_eq!(
            calibrate_min_cost(&[], 0.2, 1.0, 1.0),
            Err(CalibrationError::EmptyDataset)
        );
    }
}

use crate::model::Evidence;
use std::cmp::Ordering;
use std::collections::BTreeMap;

const RRF_K: f64 = 60.0;

pub(crate) fn fuse(channels: Vec<Vec<Evidence>>) -> Vec<Evidence> {
    let mut combined = BTreeMap::new();
    for channel in channels {
        let mut previous_score = None;
        let mut rank = 1;
        for (index, mut evidence) in channel.into_iter().enumerate() {
            if previous_score
                .is_none_or(|score: f64| score.total_cmp(&evidence.score) != Ordering::Equal)
            {
                rank = index + 1;
                previous_score = Some(evidence.score);
            }
            evidence.score = 1.0 / (RRF_K + rank as f64);
            let key = (
                evidence.path.clone(),
                evidence.start_line,
                evidence.end_line,
                evidence.content_sha.clone(),
            );
            if let Some(existing) = combined.get_mut(&key) {
                merge(existing, evidence);
            } else {
                combined.insert(key, evidence);
            }
        }
    }

    let mut evidence = combined.into_values().collect::<Vec<_>>();
    evidence.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.start_line.cmp(&right.start_line))
            .then_with(|| left.end_line.cmp(&right.end_line))
    });
    evidence
}

fn merge(existing: &mut Evidence, duplicate: Evidence) {
    existing.score += duplicate.score;
    for reason in duplicate.why {
        if !existing.why.contains(&reason) {
            existing.why.push(reason);
        }
    }
    for channel in duplicate.channels {
        if !existing.channels.contains(&channel) {
            existing.channels.push(channel);
        }
    }
    existing.channels.sort();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Channel;

    fn evidence(channel: Channel, score: f64) -> Evidence {
        Evidence::verified(
            "src/lib.rs".into(),
            1,
            3,
            "sha".into(),
            "fn marker() {}".into(),
            score,
            vec![format!("{channel:?}")],
            vec![channel],
        )
    }

    #[test]
    fn exact_cells_merge_across_channels() {
        let fused = fuse(vec![
            vec![evidence(Channel::Bm25, 3.0)],
            vec![evidence(Channel::Literal, 1.0)],
        ]);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].channels, vec![Channel::Literal, Channel::Bm25]);
        assert!(fused[0].score > 1.0 / RRF_K);
    }
}

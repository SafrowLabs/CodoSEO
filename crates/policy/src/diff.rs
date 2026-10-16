//! What changed between two policies, as the history says it: the changed keys and their values,
//! `ai.purposes.training: any → block`.

use serde_json::Value;

use crate::SitePolicy;

/// Longest summary kept, in characters.
const MAX_SUMMARY: usize = 500;
/// Longest value shown in a summary, in characters.
const MAX_VALUE: usize = 60;

/// One changed key: its dotted path and the values before and after (`None`: absent, which is
/// the default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    pub key: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

/// The keys whose values differ, in path order. Objects are compared key by key; arrays as a
/// whole.
pub fn changed_keys(before: &SitePolicy, after: &SitePolicy) -> Vec<Changed> {
    let mut out = Vec::new();
    walk(
        "",
        Some(&before.to_json()),
        Some(&after.to_json()),
        &mut out,
    );
    out
}

fn walk(path: &str, a: Option<&Value>, b: Option<&Value>, out: &mut Vec<Changed>) {
    match (a, b) {
        (Some(Value::Object(x)), Some(Value::Object(y))) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let p = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                walk(&p, x.get(k), y.get(k), out);
            }
        }
        (Some(Value::Object(x)), None) => {
            for (k, v) in x {
                walk(&format!("{path}.{k}"), Some(v), None, out);
            }
        }
        (None, Some(Value::Object(y))) => {
            for (k, v) in y {
                walk(&format!("{path}.{k}"), None, Some(v), out);
            }
        }
        (a, b) if a != b => out.push(Changed {
            key: path.to_owned(),
            before: a.cloned(),
            after: b.cloned(),
        }),
        _ => {}
    }
}

fn show(v: &Option<Value>) -> String {
    let s = match v {
        None => "default".to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    };
    if s.chars().count() > MAX_VALUE {
        let cut: String = s.chars().take(MAX_VALUE - 1).collect();
        format!("{cut}…")
    } else {
        s
    }
}

/// `ai.purposes.training: any → block; ai.probes: true → false`, or `no change`.
pub fn summary(before: &SitePolicy, after: &SitePolicy) -> String {
    let changed = changed_keys(before, after);
    if changed.is_empty() {
        return "no change".to_owned();
    }
    let mut s = changed
        .iter()
        .map(|c| format!("{}: {} → {}", c.key, show(&c.before), show(&c.after)))
        .collect::<Vec<_>>()
        .join("; ");
    if s.chars().count() > MAX_SUMMARY {
        s = s.chars().take(MAX_SUMMARY - 1).collect::<String>() + "…";
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use codoseo_geo::Stance;
    use codoseo_geo::registry::Purpose;

    #[test]
    fn the_summary_names_each_changed_key() {
        let a = SitePolicy::default();
        let mut b = a.clone();
        b.ai.intent.purposes.insert(Purpose::Training, Stance::Block);
        b.ai.probes = false;
        assert_eq!(
            summary(&a, &b),
            "ai.probes: true → false; ai.purposes.training: default → block"
        );
        assert_eq!(summary(&a, &a), "no change");
    }
}

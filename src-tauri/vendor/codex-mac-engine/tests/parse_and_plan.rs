use codex_mac_engine::{parse_appcast, plan_update, UpdateStrategy};

const FIXTURE: &str = include_str!("fixtures/appcast.xml");

#[test]
fn parses_items_and_deltas() {
    let appcast = parse_appcast(FIXTURE).unwrap();
    assert!(appcast.items.len() >= 5, "expected >=5 items");

    let latest = appcast.latest().unwrap();
    assert_eq!(latest.build, 3575);
    assert_eq!(latest.short_version, "26.602.30954");
    assert_eq!(latest.full.length, 406_581_087);
    assert_eq!(latest.deltas.len(), 5);
    assert!(latest.deltas.iter().any(|d| d.from_build == 3511));
    assert!(latest.full.ed_signature.is_some());
}

#[test]
fn picks_delta_when_available() {
    let appcast = parse_appcast(FIXTURE).unwrap();
    let plan = plan_update(&appcast, 3511).unwrap();

    assert!(!plan.up_to_date);
    assert!(matches!(
        plan.strategy,
        UpdateStrategy::Delta { from_build: 3511 }
    ));
    assert_eq!(plan.download_size, 18_260_894);
    assert_eq!(plan.full_size, 406_581_087);
    assert!(plan.savings_pct > 95.0, "savings {}", plan.savings_pct);
    assert!(plan.ed_signature.is_some());
}

#[test]
fn falls_back_to_full_outside_delta_window() {
    let appcast = parse_appcast(FIXTURE).unwrap();
    // build 3000 has no delta in the latest item's window.
    let plan = plan_update(&appcast, 3000).unwrap();
    assert!(matches!(plan.strategy, UpdateStrategy::Full));
    assert_eq!(plan.download_size, 406_581_087);
    assert!((plan.savings_pct - 0.0).abs() < f64::EPSILON);
}

#[test]
fn up_to_date_when_current_is_latest_or_newer() {
    let appcast = parse_appcast(FIXTURE).unwrap();
    assert!(plan_update(&appcast, 3575).unwrap().up_to_date);
    assert!(plan_update(&appcast, 9999).unwrap().up_to_date);
}

#[test]
fn empty_appcast_is_error() {
    let err = parse_appcast("<rss><channel></channel></rss>");
    assert!(err.is_err());
}

use serde_json::Value;
use videosift_core::task::{TaskEvent, TaskState};

const STATES: [TaskState; 10] = [
    TaskState::Queued,
    TaskState::Running,
    TaskState::PauseRequested,
    TaskState::Paused,
    TaskState::WaitingProvider,
    TaskState::CancelRequested,
    TaskState::Cancelled,
    TaskState::Succeeded,
    TaskState::Failed,
    TaskState::Interrupted,
];

#[test]
fn every_state_event_pair_matches_the_lifecycle_table() {
    use TaskEvent::*;
    use TaskState::*;

    let events = [
        Dispatch,
        RequestPause,
        PauseAcknowledged,
        Resume,
        RequestCancel,
        CancellationAcknowledged,
        Complete,
        Fail,
        ProviderRateLimited,
        RecoverAfterRestart,
    ];
    // Columns follow events; rows follow STATES. None denotes a rejected event.
    let expected = [
        [
            Some(Running),
            Some(Paused),
            None,
            Some(Queued),
            Some(Cancelled),
            None,
            None,
            None,
            None,
            Some(Queued),
        ],
        [
            None,
            Some(PauseRequested),
            None,
            Some(Running),
            Some(CancelRequested),
            None,
            Some(Succeeded),
            Some(Failed),
            Some(WaitingProvider),
            Some(Interrupted),
        ],
        [
            None,
            Some(PauseRequested),
            Some(Paused),
            None,
            Some(CancelRequested),
            None,
            Some(Succeeded),
            Some(Failed),
            Some(WaitingProvider),
            Some(Interrupted),
        ],
        [
            None,
            Some(Paused),
            None,
            Some(Queued),
            Some(Cancelled),
            None,
            None,
            None,
            None,
            Some(Paused),
        ],
        [
            None,
            None,
            None,
            Some(Queued),
            Some(Cancelled),
            None,
            None,
            None,
            None,
            Some(WaitingProvider),
        ],
        [
            None,
            None,
            None,
            None,
            Some(CancelRequested),
            Some(Cancelled),
            None,
            None,
            None,
            Some(Interrupted),
        ],
        [
            None,
            None,
            None,
            None,
            Some(Cancelled),
            None,
            None,
            None,
            None,
            Some(Cancelled),
        ],
        [
            None,
            None,
            None,
            None,
            Some(Succeeded),
            None,
            None,
            None,
            None,
            Some(Succeeded),
        ],
        [
            None,
            None,
            None,
            None,
            Some(Failed),
            None,
            None,
            None,
            None,
            Some(Failed),
        ],
        [
            None,
            None,
            None,
            Some(Queued),
            Some(Cancelled),
            None,
            None,
            None,
            None,
            Some(Interrupted),
        ],
    ];
    for (state, row) in STATES.into_iter().zip(expected) {
        for (event, expected_state) in events.into_iter().zip(row) {
            let result = state.transition(event);
            assert_eq!(
                result.as_ref().ok().copied(),
                expected_state,
                "{state:?} + {event:?}"
            );
            if let Err(error) = result {
                assert_eq!(error.state, state);
                assert_eq!(error.event, event);
            }
        }
    }
}

#[test]
fn pause_waits_for_a_step_boundary_and_resume_requeues() {
    use TaskEvent::*;
    let mut state = TaskState::Queued;
    for event in [Dispatch, RequestPause, RequestPause] {
        state = state.transition(event).unwrap();
    }
    assert_eq!(state, TaskState::PauseRequested);
    assert!(state.transition(Dispatch).is_err());
    state = state.transition(PauseAcknowledged).unwrap();
    assert_eq!(state, TaskState::Paused);
    state = state.transition(Resume).unwrap();
    assert_eq!(state, TaskState::Queued);
    assert_eq!(state.transition(Dispatch).unwrap(), TaskState::Running);
}

#[test]
fn cancellation_and_completion_respect_both_event_orders() {
    use TaskEvent::*;
    let cancelling = TaskState::Running.transition(RequestCancel).unwrap();
    assert_eq!(cancelling.transition(RequestCancel).unwrap(), cancelling);
    for late_event in [Complete, Fail, ProviderRateLimited, PauseAcknowledged] {
        assert!(cancelling.transition(late_event).is_err());
    }
    let cancelled = cancelling.transition(CancellationAcknowledged).unwrap();
    assert_eq!(cancelled, TaskState::Cancelled);
    assert_eq!(cancelled.transition(RequestCancel).unwrap(), cancelled);

    let completed = TaskState::Running.transition(Complete).unwrap();
    assert_eq!(
        completed.transition(RequestCancel).unwrap(),
        TaskState::Succeeded
    );
    assert!(completed.transition(Fail).is_err());
}

#[test]
fn provider_limit_requires_explicit_resume_even_after_restart() {
    use TaskEvent::*;
    let waiting = TaskState::Running.transition(ProviderRateLimited).unwrap();
    assert_eq!(waiting, TaskState::WaitingProvider);
    assert!(waiting.transition(Dispatch).is_err());
    assert_eq!(waiting.transition(RecoverAfterRestart).unwrap(), waiting);
    assert_eq!(waiting.transition(Resume).unwrap(), TaskState::Queued);
}

#[test]
fn recovery_interrupts_active_work_and_requires_explicit_resume() {
    use TaskEvent::*;
    for state in [
        TaskState::Running,
        TaskState::PauseRequested,
        TaskState::CancelRequested,
    ] {
        let recovered = state.transition(RecoverAfterRestart).unwrap();
        assert_eq!(recovered, TaskState::Interrupted);
        assert!(!recovered.is_terminal());
        assert!(recovered.transition(Dispatch).is_err());
        assert!(recovered.transition(Complete).is_err());
        assert_eq!(
            recovered.transition(RecoverAfterRestart).unwrap(),
            recovered
        );
        assert_eq!(recovered.transition(Resume).unwrap(), TaskState::Queued);
    }
}

#[test]
fn terminal_states_never_change() {
    use TaskEvent::*;
    for state in [
        TaskState::Cancelled,
        TaskState::Succeeded,
        TaskState::Failed,
    ] {
        assert!(state.is_terminal());
        for event in [
            Dispatch,
            RequestPause,
            PauseAcknowledged,
            Resume,
            RequestCancel,
            CancellationAcknowledged,
            Complete,
            Fail,
            ProviderRateLimited,
            RecoverAfterRestart,
        ] {
            if let Ok(next) = state.transition(event) {
                assert_eq!(next, state);
            }
        }
    }
}

#[test]
fn state_wire_values_match_schema_and_shared_fixtures() {
    let schema: Value =
        serde_json::from_str(include_str!("../../../contracts/domain.schema.json")).unwrap();
    let values = schema["$defs"]["ProcessingTask"]["properties"]["state"]["enum"]
        .as_array()
        .unwrap();
    let actual: Vec<Value> = STATES
        .iter()
        .map(|state| serde_json::to_value(state).unwrap())
        .collect();
    assert_eq!(&actual, values);
    for (state, value) in STATES.into_iter().zip(values) {
        assert_eq!(
            serde_json::from_value::<TaskState>(value.clone()).unwrap(),
            state
        );
    }
    let fixtures: Value =
        serde_json::from_str(include_str!("../../../contracts/examples/valid.json")).unwrap();
    let mut count = 0;
    for fixture in fixtures.as_array().unwrap() {
        let instance = &fixture["instance"];
        let task = if instance.get("state").is_some() {
            instance
        } else {
            &instance["result"]
        };
        if let Some(value) = task.get("state") {
            let state: TaskState = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(state).unwrap(), *value);
            count += 1;
        }
    }
    assert!(count > 0);
    for value in ["Running", "running\n", "unknown", ""] {
        assert!(serde_json::from_value::<TaskState>(Value::String(value.into())).is_err());
    }
}

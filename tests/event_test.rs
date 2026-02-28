use vclaw::event::{Event, EventBus, VoiceStatus};

#[tokio::test]
async fn test_event_bus_send_receive() {
    let bus = EventBus::new(32);
    let mut rx = bus.subscribe();
    let tx = bus.sender();

    tx.send(Event::UserSaid("hello".into())).unwrap();
    let event = rx.recv().await.unwrap();
    assert!(matches!(event, Event::UserSaid(s) if s == "hello"));
}

#[tokio::test]
async fn test_event_bus_multiple_subscribers() {
    let bus = EventBus::new(32);
    let mut rx1 = bus.subscribe();
    let mut rx2 = bus.subscribe();
    let tx = bus.sender();

    tx.send(Event::VoiceStatus(VoiceStatus::Listening)).unwrap();

    let e1 = rx1.recv().await.unwrap();
    let e2 = rx2.recv().await.unwrap();
    assert!(matches!(e1, Event::VoiceStatus(VoiceStatus::Listening)));
    assert!(matches!(e2, Event::VoiceStatus(VoiceStatus::Listening)));
}

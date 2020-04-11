use std::time::Duration;

use futures::{
    channel::oneshot,
    future::{self, Either},
    SinkExt, StreamExt,
};
use serde::{Deserialize, Serialize};

use turbulence::{
    buffer::BufferPacketPool,
    message_channels::{MessageChannelMode, MessageChannelSettings, MessageChannelsBuilder},
    packet_multiplexer::{MuxPacketPool, PacketMultiplexer},
    reliable_channel,
    runtime::Runtime,
};

mod util;

use self::util::{SimpleBufferPool, SimpleRuntime};

#[derive(Serialize, Deserialize)]
struct Message1(i32);

const MESSAGE1_SETTINGS: MessageChannelSettings = MessageChannelSettings {
    channel: 0,
    channel_mode: MessageChannelMode::Reliable {
        reliability_settings: reliable_channel::Settings {
            bandwidth: 4096,
            recv_window_size: 1024,
            send_window_size: 1024,
            burst_bandwidth: 1024,
            init_send: 512,
            wakeup_time: Duration::from_millis(100),
            initial_rtt: Duration::from_millis(200),
            max_rtt: Duration::from_secs(2),
            rtt_update_factor: 0.1,
            rtt_resend_factor: 1.5,
        },
        max_message_len: 1024,
    },
    message_buffer_size: 8,
    packet_buffer_size: 8,
};

#[derive(Serialize, Deserialize)]
struct Message2(i32);

const MESSAGE2_SETTINGS: MessageChannelSettings = MessageChannelSettings {
    channel: 1,
    channel_mode: MessageChannelMode::Unreliable,
    message_buffer_size: 8,
    packet_buffer_size: 8,
};

#[test]
fn test_message_channels() {
    let mut runtime = SimpleRuntime::new();
    let pool = MuxPacketPool::new(BufferPacketPool::new(SimpleBufferPool(32)));

    let mut multiplexer_a = PacketMultiplexer::new();
    let mut builder_a = MessageChannelsBuilder::new(runtime.handle(), pool.clone());
    builder_a.register::<Message1>(MESSAGE1_SETTINGS).unwrap();
    builder_a.register::<Message2>(MESSAGE2_SETTINGS).unwrap();
    let mut channels_a = builder_a.build(&mut multiplexer_a);

    let mut multiplexer_b = PacketMultiplexer::new();
    let mut builder_b = MessageChannelsBuilder::new(runtime.handle(), pool.clone());
    builder_b.register::<Message1>(MESSAGE1_SETTINGS).unwrap();
    builder_b.register::<Message2>(MESSAGE2_SETTINGS).unwrap();
    let mut channels_b = builder_b.build(&mut multiplexer_b);

    runtime.spawn(async move {
        let (mut a_incoming, mut a_outgoing) = multiplexer_a.start();
        let (mut b_incoming, mut b_outgoing) = multiplexer_b.start();
        loop {
            match future::select(a_outgoing.next(), b_outgoing.next()).await {
                Either::Left((Some(packet), _)) => {
                    b_incoming.send(packet).await.unwrap();
                }
                Either::Right((Some(packet), _)) => {
                    a_incoming.send(packet).await.unwrap();
                }
                Either::Left((None, _)) | Either::Right((None, _)) => break,
            }
        }
    });

    let (is_done_send, mut is_done_recv) = oneshot::channel();
    runtime.spawn(async move {
        channels_a.async_send(Message1(42)).await;
        channels_a.flush::<Message1>();
        assert_eq!(channels_b.async_recv::<Message1>().await.0, 42);

        channels_a.async_send(Message2(13)).await;
        channels_a.flush::<Message2>();
        assert_eq!(channels_b.async_recv::<Message2>().await.0, 13);

        channels_a.async_send(Message1(20)).await;
        channels_a.async_send(Message2(30)).await;
        channels_a.async_send(Message1(21)).await;
        channels_a.async_send(Message2(31)).await;
        channels_a.async_send(Message1(22)).await;
        channels_a.async_send(Message2(32)).await;
        channels_a.flush::<Message1>();
        channels_a.flush::<Message2>();

        assert_eq!(channels_b.async_recv::<Message1>().await.0, 20);
        assert_eq!(channels_b.async_recv::<Message1>().await.0, 21);
        assert_eq!(channels_b.async_recv::<Message1>().await.0, 22);

        assert_eq!(channels_b.async_recv::<Message2>().await.0, 30);
        assert_eq!(channels_b.async_recv::<Message2>().await.0, 31);
        assert_eq!(channels_b.async_recv::<Message2>().await.0, 32);

        is_done_send.send(()).unwrap();
    });

    for _ in 0..100_000 {
        if is_done_recv.try_recv().unwrap().is_some() {
            return;
        }

        runtime.run_until_stalled();
        runtime.advance_time(50);
    }

    panic!("didn't finish in time");
}

//! Same-process iceoryx2 integration tests.
//!
//! Validates that iceoryx2 pub/sub and request/response work correctly
//! on the current platform (Windows). These tests run publisher and
//! subscriber within the same process — iceoryx2 supports this mode.

use iceoryx2::prelude::*;
use ingot_ipc::types::{
    FixedId, IpcCommand, IpcCommandResponse, IpcNavUpdate, IpcOrderRequest, IpcTickerUpdate,
};
use ingot_primitives::{Amount, Currency, OrderSide, OrderType, Price, Quantity, Symbol};
use rust_decimal::dec;

/// Helper to create a unique service name per test to avoid collisions.
fn svc(suffix: &str) -> anyhow::Result<ServiceName> {
    let s = format!("IngotTest/{suffix}/{}", std::process::id());
    let name: ServiceName = s.as_str().try_into()?;
    Ok(name)
}

#[test]
fn test_pubsub_ticker_round_trip() -> anyhow::Result<()> {
    let node = NodeBuilder::new().create::<ipc::Service>()?;

    let service = node
        .service_builder(&svc("ticker")?)
        .publish_subscribe::<IpcTickerUpdate>()
        .open_or_create()?;

    let publisher = service.publisher_builder().create()?;
    let subscriber = service.subscriber_builder().create()?;

    let payload = IpcTickerUpdate {
        symbol: Symbol::new("BTC-USD"),
        price: Price::from(dec!(50000)),
    };

    let sample = publisher.loan_uninit()?;
    let sample = sample.write_payload(payload);
    sample.send()?;

    let received = subscriber.receive()?;
    let received = received.ok_or_else(|| anyhow::anyhow!("no message received"))?;

    assert_eq!(received.symbol, Symbol::new("BTC-USD"));
    assert_eq!(received.price, Price::from(dec!(50000)));
    Ok(())
}

#[test]
fn test_pubsub_nav_round_trip() -> anyhow::Result<()> {
    let node = NodeBuilder::new().create::<ipc::Service>()?;

    let service = node
        .service_builder(&svc("nav")?)
        .publish_subscribe::<IpcNavUpdate>()
        .open_or_create()?;

    let publisher = service.publisher_builder().create()?;
    let subscriber = service.subscriber_builder().create()?;

    let payload = IpcNavUpdate {
        amount: Amount::from(dec!(10000.50)),
        currency: Currency::usd(),
    };

    let sample = publisher.loan_uninit()?;
    let sample = sample.write_payload(payload);
    sample.send()?;

    let received = subscriber.receive()?;
    let received = received.ok_or_else(|| anyhow::anyhow!("no message received"))?;

    assert_eq!(received.amount, Amount::from(dec!(10000.50)));
    assert_eq!(received.currency, Currency::usd());
    Ok(())
}

#[test]
fn test_request_response_heartbeat() -> anyhow::Result<()> {
    let node = NodeBuilder::new().create::<ipc::Service>()?;

    let service = node
        .service_builder(&svc("cmd")?)
        .request_response::<IpcCommand, IpcCommandResponse>()
        .open_or_create()?;

    let client = service.client_builder().create()?;
    let server = service.server_builder().create()?;

    // Client sends a heartbeat command
    let pending = client.send_copy(IpcCommand::Heartbeat)?;

    // Server receives and responds
    let active_request = server
        .receive()?
        .ok_or_else(|| anyhow::anyhow!("no request received"))?;
    assert!(matches!(*active_request, IpcCommand::Heartbeat));

    let response = active_request.loan_uninit()?;
    let response = response.write_payload(IpcCommandResponse::Heartbeat { uptime_secs: 42 });
    response.send()?;

    // Client receives response
    let resp = pending
        .receive()?
        .ok_or_else(|| anyhow::anyhow!("no response received"))?;
    match *resp {
        IpcCommandResponse::Heartbeat { uptime_secs } => {
            assert_eq!(uptime_secs, 42);
        }
        ref other => anyhow::bail!("expected Heartbeat, got {other:?}"),
    }
    Ok(())
}

#[test]
fn test_request_response_place_order() -> anyhow::Result<()> {
    let node = NodeBuilder::new().create::<ipc::Service>()?;

    let service = node
        .service_builder(&svc("order")?)
        .request_response::<IpcCommand, IpcCommandResponse>()
        .open_or_create()?;

    let client = service.client_builder().create()?;
    let server = service.server_builder().create()?;

    let order_req = IpcOrderRequest {
        symbol: Symbol::new("ETH-USD"),
        side: OrderSide::Buy,
        order_type: OrderType::Market,
        quantity: Quantity::from(dec!(5)),
        has_price: false,
        price: Price::from(dec!(0)),
        validate_only: false,
    };
    let pending = client.send_copy(IpcCommand::PlaceOrder(order_req))?;

    // Server receives
    let active_request = server
        .receive()?
        .ok_or_else(|| anyhow::anyhow!("no request received"))?;

    if let IpcCommand::PlaceOrder(req) = *active_request {
        assert_eq!(req.symbol, Symbol::new("ETH-USD"));
        assert_eq!(req.side, OrderSide::Buy);
    } else {
        anyhow::bail!("expected PlaceOrder, got {:?}", *active_request);
    }

    // Server responds with error (simulated)
    let response = active_request.loan_uninit()?;
    let response = response.write_payload(IpcCommandResponse::Error {
        code: 503,
        message: FixedId::from_slice("exchange unavailable"),
    });
    response.send()?;

    // Client receives error
    let resp = pending
        .receive()?
        .ok_or_else(|| anyhow::anyhow!("no response received"))?;
    match *resp {
        IpcCommandResponse::Error { code, ref message } => {
            assert_eq!(code, 503);
            assert_eq!(message.as_str(), "exchange unavailable");
        }
        ref other => anyhow::bail!("expected Error, got {other:?}"),
    }
    Ok(())
}

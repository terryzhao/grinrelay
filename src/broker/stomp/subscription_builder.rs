use super::frame::Frame;
use super::header::HeaderList;
use super::option_setter::OptionSetter;
use super::session::{OutstandingReceipt, ReceiptRequest, Session};
use super::subscription::{AckMode, Subscription};

pub struct SubscriptionBuilder<'a, T: 'static> {
	pub session: &'a mut Session<T>,
	pub destination: String,
	pub ack_mode: AckMode,
	pub headers: HeaderList,
	pub receipt_request: Option<ReceiptRequest>,
}

impl<'a, T> SubscriptionBuilder<'a, T>
where
	T: tokio_io::AsyncWrite + tokio_io::AsyncRead + Send + 'static,
{
	pub fn new(session: &'a mut Session<T>, destination: String) -> Self {
		SubscriptionBuilder {
			session: session,
			destination: destination,
			ack_mode: AckMode::Auto,
			headers: HeaderList::new(),
			receipt_request: None,
		}
	}

	#[allow(dead_code)]
	pub fn start(mut self) -> String {
		let next_id = self.session.generate_subscription_id();
		let subscription = Subscription::new(
			next_id,
			&self.destination,
			self.ack_mode,
			self.headers.clone(),
		);
		let mut subscribe_frame =
			Frame::subscribe(&subscription.id, &self.destination, self.ack_mode);

		subscribe_frame.headers.concat(&mut self.headers);

		self.session.send_frame(subscribe_frame.clone());

		debug!(
			"Registering callback for subscription id '{}' from builder",
			subscription.id
		);
		let id_to_return = subscription.id.to_string();
		self.session
			.state
			.subscriptions
			.insert(subscription.id.to_string(), subscription);
		if self.receipt_request.is_some() {
			let request = self.receipt_request.unwrap();
			self.session
				.state
				.outstanding_receipts
				.insert(request.id, OutstandingReceipt::new(subscribe_frame.clone()));
		}
		id_to_return
	}

	pub fn with<O>(self, option_setter: O) -> SubscriptionBuilder<'a, T>
	where
		O: OptionSetter<SubscriptionBuilder<'a, T>>,
	{
		option_setter.set_option(self)
	}
}

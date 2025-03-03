// Copyright 2018 The Vault713 Developers
// Modifications Copyright 2019 The Gotts Developers
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#[macro_use]
extern crate log;
#[macro_use]
extern crate futures;

mod broker;
mod server;

use crate::broker::Broker;
use crate::server::AsyncServer;
use colored::*;
use grinrelaylib::types::{set_running_mode, ChainTypes};
use parking_lot::Mutex;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::default::Default;
use std::net::{TcpListener, ToSocketAddrs};
use std::sync::Arc;
use std::thread;

use std::fs::File;
use std::io::Read;
use std::rc::Rc;

use openssl::pkey::PKey;
use openssl::ssl::{SslAcceptor, SslMethod};
use openssl::x509::X509;

use amqp::protocol::basic;
use amqp::AMQPScheme;
use amqp::TableEntry;
use amqp::{Basic, Channel, Options, Session, Table};

extern crate serde_derive;
extern crate serde_json;

use serde_json::Value;
use uuid::Uuid;

fn read_file(name: &str) -> std::io::Result<Vec<u8>> {
	let mut file = File::open(name)?;
	let mut buf = Vec::new();
	file.read_to_end(&mut buf)?;
	Ok(buf)
}

fn initial_consumers(login: String, password: String) -> HashMap<String, Vec<String>> {
	let mut map = HashMap::new();

	let client = reqwest::Client::new();
	let mut resp = client
		.get("http://localhost:15672/api/consumers")
		.basic_auth(login, Some(password))
		.send()
		.unwrap();

	if resp.status().is_success() {
		let data: Value = serde_json::from_str(resp.text().unwrap().as_str()).unwrap();
		let obj_array = data.as_array().unwrap();

		for obj in obj_array {
			let queue = obj.get("queue").unwrap().as_object().unwrap();
			let name = queue.get("name").unwrap().as_str().unwrap();
			let str_name: String = String::from(name);
			let len = str_name.len();
			let key = str_name.clone()[len - 6..].to_owned();

			match map.entry(key) {
				Entry::Vacant(e) => {
					e.insert(vec![str_name]);
				}
				Entry::Occupied(mut e) => {
					e.get_mut().push(str_name);
				}
			}
		}
	}

	if !map.is_empty() {
		for (key, vec_val) in map.iter() {
			let v1_iter = vec_val.iter();
			for val in v1_iter {
				info!("{}: {}", key, val);
			}
		}
	}

	map
}

fn rabbit_consumer_monitor(
	consumers: Arc<Mutex<HashMap<String, Vec<String>>>>,
	login: String,
	password: String,
) {
	let map = initial_consumers(login.clone(), password.clone());
	for (key, value) in map.to_owned() {
		consumers.lock().insert(key, value);
	}

	info!("rabbit_consumer_monitor start");
	let options = Options {
		host: "127.0.0.1".to_string(),
		port: 5672,
		vhost: "/".to_string(),
		login,
		password,
		frame_max_limit: 131072,
		channel_max_limit: 65535,
		locale: "en_US".to_string(),
		scheme: AMQPScheme::AMQP,
		properties: Table::new(),
	};

	let mut session = Session::new(options).ok().expect("Can't create session");
	let mut channel = session
		.open_channel(1)
		.ok()
		.expect("Error opening channel 1");
	info!("Opened channel: {:?}", channel.id);

	let queue_name = format!(
		"{}-{}-consumer-notify",
		gethostname::gethostname().into_string().unwrap(),
		Uuid::new_v4()
	);
	let mut args = Table::new();
	args.insert("x-expires".to_owned(), TableEntry::LongUint(86400000u32));
	let queue_declare =
		channel.queue_declare(queue_name.clone(), false, false, false, false, false, args);

	if queue_declare.is_err() {
		error!("grin relay consumer queue failed to declare!");
		std::process::exit(1);
	} else {
		info!("Queue declared: {:?}", queue_declare.unwrap());
	}

	let bind_result = channel.queue_bind(
		queue_name.clone(),
		"amq.rabbitmq.event".to_owned(),
		"consumer.*".to_owned(),
		false,
		Table::new(),
	);
	if bind_result.is_err() {
		error!("grin relay consumer queue failed to bind!");
		std::process::exit(1);
	} else {
		info!("queue bind successfully");
	}

	let closure_consumer = move |_chan: &mut Channel,
	                             deliver: basic::Deliver,
	                             headers: basic::BasicProperties,
	                             _data: Vec<u8>| {
		if deliver.routing_key == "consumer.created" {
			let header = headers.to_owned().headers.unwrap();
			let queue = match header.get("queue").unwrap() {
				TableEntry::LongString(val) => val.to_string(),
				_ => String::new(),
			};

			if queue.starts_with("gn1") || queue.starts_with("tn1") {
				info!("consumer.created ---- {}", queue);

				let tail = queue.len().saturating_sub(6);
				let key = queue[tail..].to_string();
				match consumers.lock().entry(key) {
					Entry::Vacant(e) => {
						e.insert(vec![queue]);
					}
					Entry::Occupied(mut e) => {
						e.get_mut().push(queue);
					}
				}
			}
		}

		if deliver.routing_key == "consumer.deleted" {
			let header = headers.to_owned().headers.unwrap();

			let queue = match header.get("queue").unwrap() {
				TableEntry::LongString(val) => val.to_string(),
				_ => String::new(),
			};

			if queue.starts_with("gn1") || queue.starts_with("tn1") {
				info!("consumer.deleted ---- {}", queue);

				let tail = queue.len().saturating_sub(6);
				let key = &queue[tail..];
				if consumers.lock().contains_key(key) {
					consumers.lock().remove(key);
				}
			}
		}
	};
	let consumer_name = channel.basic_consume(
		closure_consumer,
		queue_name,
		"".to_owned(),
		false,
		true,
		false,
		false,
		Table::new(),
	);
	info!("Starting consumer {:?}", consumer_name);

	thread::spawn(move || {
		channel.start_consuming();

		channel.close(200, "Bye").unwrap();
		session.close(200, "Good Bye");
		info!("rabbit_consumer_monitor exit");
	});
}

// include build information
pub mod built_info {
	include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

pub fn info_strings() -> (String, String) {
	(
		format!(
			"This is Grin version {}{}, built for {} by {}.",
			built_info::PKG_VERSION,
			built_info::GIT_VERSION.map_or_else(|| "".to_owned(), |v| format!(" (git {})", v)),
			built_info::TARGET,
			built_info::RUSTC_VERSION,
		)
		.to_string(),
		format!(
			"Built with profile \"{}\", features \"{}\".",
			built_info::PROFILE,
			built_info::FEATURES_STR,
		)
		.to_string(),
	)
}

fn log_build_info() {
	let (basic_info, detailed_info) = info_strings();
	info!("{}", basic_info);
	debug!("{}", detailed_info);
}

fn main() {
	env_logger::init();

	log_build_info();

	let broker_uri = std::env::var("BROKER_URI")
		.unwrap_or_else(|_| "127.0.0.1:61613".to_string())
		.to_socket_addrs()
		.unwrap()
		.next();

	let grinrelay_protocol_unsecure = std::env::var("GRINRELAY_PROTOCOL_UNSECURE")
		.map(|_| true)
		.unwrap_or(false);

	let acceptor = if !grinrelay_protocol_unsecure {
		info!("{}", "wss enabled".bright_green());
		let cert_file = std::env::var("CERT")
			.unwrap_or("/etc/grinrelay/tls/server_certificate.pem".to_string());
		let key_file =
			std::env::var("KEY").unwrap_or("/etc/grinrelay/tls/server_key.pem".to_string());

		let cert = {
			let data = read_file(cert_file.as_str()).expect("cert_file not found");
			X509::from_pem(data.as_ref()).unwrap()
		};

		let pkey = {
			let data = read_file(key_file.as_str()).expect("key_file not found");
			PKey::private_key_from_pem(data.as_ref()).unwrap()
		};

		Some(Rc::new({
			let mut builder = SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
			builder.set_private_key(&pkey).unwrap();
			builder.set_certificate(&cert).unwrap();

			builder.build()
		}))
	} else {
		None
	};

	let username = std::env::var("BROKER_USERNAME").unwrap_or("guest".to_string());
	let password = std::env::var("BROKER_PASSWORD").unwrap_or("guest".to_string());

	let grinrelay_domain = std::env::var("GRINRELAY_DOMAIN").unwrap_or("127.0.0.1".to_string());
	let grinrelay_port = std::env::var("GRINRELAY_PORT").unwrap_or("13420".to_string());
	let grinrelay_port =
		u16::from_str_radix(&grinrelay_port, 10).expect("invalid GRINRELAY_PORT given!");

	let is_mainnet = std::env::var("GRINRELAY_IS_MAINNET")
		.map(|_| true)
		.unwrap_or(false);
	if is_mainnet {
		set_running_mode(ChainTypes::Mainnet);
	} else {
		set_running_mode(ChainTypes::Floonet);
	}

	if broker_uri.is_none() {
		error!("could not resolve broker uri!");
		panic!();
	}

	let consumers = Arc::new(Mutex::new(HashMap::new()));
	let rabbit_consumers = consumers.clone();
	let async_consumers = consumers.clone();
	rabbit_consumer_monitor(rabbit_consumers, username.clone(), password.clone());

	let broker_uri = broker_uri.unwrap();
	let bind_address =
		std::env::var("BIND_ADDRESS").unwrap_or_else(|_| "0.0.0.0:13420".to_string());

	info!("Broker URI: {}", broker_uri);
	info!("Bind address: {}", bind_address);

	let mut broker = Broker::new(broker_uri, username, password, consumers);
	let sender = broker.start().expect("failed initiating broker session");
	let response_handlers_sender = AsyncServer::init();

	thread::spawn(|| {
		// for server selection service only
		let listener = TcpListener::bind("0.0.0.0:3419").unwrap();

		// accept connections and process them serially
		for stream in listener.incoming() {
			if let Ok(stream) = stream {
				trace!("server selection from {}", stream.peer_addr().unwrap());
			}
		}
	});

	ws::Builder::new()
		.with_settings(ws::Settings {
			encrypt_server: !grinrelay_protocol_unsecure,
			..ws::Settings::default()
		})
		.build(|out: ws::Sender| {
			AsyncServer::new(
				out,
				sender.clone(),
				response_handlers_sender.clone(),
				&grinrelay_domain,
				grinrelay_port,
				grinrelay_protocol_unsecure,
				acceptor.clone(),
				async_consumers.clone(),
			)
		})
		.unwrap()
		.listen(&bind_address[..])
		.unwrap();
}

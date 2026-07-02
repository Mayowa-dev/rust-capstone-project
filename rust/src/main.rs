#![allow(unused)]
use bitcoincore_rpc::bitcoin::Amount;
use bitcoincore_rpc::{Auth, Client, RpcApi};
use serde_json::{json, Value};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

const RPC_URL: &str = "http://127.0.0.1:18443";
const RPC_USER: &str = "alice";
const RPC_PASS: &str = "password";

fn wallet_client(wallet_name: &str) -> bitcoincore_rpc::Result<Client> {
    Client::new(
        &format!("{}/wallet/{}", RPC_URL, wallet_name),
        Auth::UserPass(RPC_USER.to_owned(), RPC_PASS.to_owned()),
    )
}

fn ensure_wallet(root_rpc: &Client, wallet_name: &str) -> bitcoincore_rpc::Result<Client> {
    let loaded_wallets: Vec<String> = root_rpc.call("listwallets", &[])?;
    if loaded_wallets.iter().any(|name| name == wallet_name) {
        return wallet_client(wallet_name);
    }

    let wallet_dir: Value = root_rpc.call("listwalletdir", &[])?;
    let wallet_exists = wallet_dir
        .get("wallets")
        .and_then(|wallets| wallets.as_array())
        .map(|wallets| {
            wallets.iter().any(|entry| {
                entry
                    .get("name")
                    .and_then(|name| name.as_str())
                    .map(|name| name == wallet_name)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);

    if wallet_exists {
        root_rpc.call::<Value>("loadwallet", &[json!(wallet_name)])?;
    } else {
        root_rpc.call::<Value>("createwallet", &[json!(wallet_name)])?;
    }

    wallet_client(wallet_name)
}

fn address_from_script(script_pub_key: &Value) -> Option<String> {
    script_pub_key
        .get("address")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .or_else(|| {
            script_pub_key
                .get("addresses")
                .and_then(|value| value.as_array())
                .and_then(|addresses| addresses.first())
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
}

fn mine_until_spendable(
    root_rpc: &Client,
    wallet_rpc: &Client,
    reward_address: &str,
) -> bitcoincore_rpc::Result<(u64, Amount)> {
    let mut blocks_mined = 0;
    let mut balance: Amount = wallet_rpc.call("getbalance", &[])?;

    while balance.to_btc() <= 0.0 {
        let new_blocks: Vec<String> = root_rpc.call(
            "generatetoaddress",
            &[json!(1), json!(reward_address)],
        )?;
        blocks_mined += new_blocks.len() as u64;
        balance = wallet_rpc.call("getbalance", &[])?;
    }

    Ok((blocks_mined, balance))
}

fn main() -> bitcoincore_rpc::Result<()> {
    let root_rpc = Client::new(
        RPC_URL,
        Auth::UserPass(RPC_USER.to_owned(), RPC_PASS.to_owned()),
    )?;

    let blockchain_info = root_rpc.get_blockchain_info()?;
    println!("Connected to the node: {:?}", blockchain_info);

    let miner_rpc = ensure_wallet(&root_rpc, "Miner")?;
    let trader_rpc = ensure_wallet(&root_rpc, "Trader")?;

    let reward_address: String = miner_rpc.call("getnewaddress", &[json!("Mining Reward")])?;
    println!("Miner reward address: {}", reward_address);

    let (blocks_mined, miner_balance) = mine_until_spendable(&root_rpc, &miner_rpc, &reward_address)?;
    println!("It took {} block(s) to make the Miner wallet spendable.", blocks_mined);
    println!("Miner wallet balance: {:.8} BTC", miner_balance.to_btc());

    // Coinbase rewards only become spendable after enough confirmations, so the balance stays unavailable at first.
    println!("That is why the balance appears locked at the start.");

    let trader_address: String = trader_rpc.call("getnewaddress", &[json!("Received")])?;
    println!("Trader receiving address: {}", trader_address);

    let txid: String = miner_rpc.call("sendtoaddress", &[json!(trader_address.clone()), json!(20.0)])?;
    println!("Transaction created: {}", txid);

    let mempool_entry: Value = root_rpc.call("getmempoolentry", &[json!(txid.clone())])?;
    println!("Current mempool entry: {}", mempool_entry);

    let mined_block: Vec<String> = root_rpc.call("generatetoaddress", &[json!(1), json!(reward_address)])?;
    let confirmed_block_hash = mined_block.first().cloned().unwrap_or_default();
    println!("The transaction was confirmed in block: {}", confirmed_block_hash);

    let tx: Value = miner_rpc.call("gettransaction", &[json!(txid.clone()), json!(null), json!(true)])?;
    let decoded = tx.get("decoded").and_then(|value| value.as_object()).unwrap();
    let vins = decoded.get("vin").and_then(|value| value.as_array()).unwrap();
    let vouts = decoded.get("vout").and_then(|value| value.as_array()).unwrap();

    let input = vins.first().unwrap();
    let prev_txid = input.get("txid").and_then(|value| value.as_str()).unwrap();
    let prev_vout_index = input.get("vout").and_then(|value| value.as_u64()).unwrap() as usize;

    let prev_tx: Value = root_rpc.call("getrawtransaction", &[json!(prev_txid), json!(true)])?;
    let prev_vout = prev_tx
        .get("vout")
        .and_then(|value| value.as_array())
        .and_then(|outputs| outputs.get(prev_vout_index))
        .unwrap();

    let miner_input_address = prev_vout
        .get("scriptPubKey")
        .and_then(address_from_script)
        .unwrap();
    let miner_input_amount = prev_vout.get("value").and_then(|value| value.as_f64()).unwrap();

    let trader_output = vouts.iter().find(|output| {
        output
            .get("scriptPubKey")
            .and_then(address_from_script)
            .as_deref()
            .map(|addr| addr == trader_address)
            .unwrap_or(false)
    }).unwrap();
    let trader_output_amount = trader_output.get("value").and_then(|value| value.as_f64()).unwrap();

    let change_output = vouts.iter().find(|output| {
        output
            .get("scriptPubKey")
            .and_then(address_from_script)
            .as_deref()
            .map(|addr| addr != trader_address)
            .unwrap_or(false)
    }).unwrap();
    let miner_change_address = change_output
        .get("scriptPubKey")
        .and_then(address_from_script)
        .unwrap();
    let miner_change_amount = change_output.get("value").and_then(|value| value.as_f64()).unwrap();

    let fee = tx.get("fee").and_then(|value| value.as_f64()).unwrap();
    let block_height = tx.get("blockheight").and_then(|value| value.as_i64()).unwrap();
    let block_hash = tx.get("blockhash").and_then(|value| value.as_str()).unwrap();

    let cwd = std::env::current_dir()?;
    let out_path = if cwd.ends_with("rust") {
        cwd.parent().map(|path| path.join("out.txt")).unwrap_or_else(|| cwd.join("out.txt"))
    } else {
        cwd.join("out.txt")
    };

    let mut file = File::create(&out_path)?;
    for line in [
        txid,
        miner_input_address,
        miner_input_amount.to_string(),
        trader_address,
        trader_output_amount.to_string(),
        miner_change_address,
        miner_change_amount.to_string(),
        fee.to_string(),
        block_height.to_string(),
        block_hash.to_string(),
    ] {
        writeln!(file, "{}", line)?;
    }

    println!("Wrote the transaction summary to {:?}", out_path);
    Ok(())
}

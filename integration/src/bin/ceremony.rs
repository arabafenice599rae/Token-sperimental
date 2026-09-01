//! Prova generale del deploy su un validator reale.
//!
//! Fa quello che litesvm non può fare: misura le compute unit effettivamente
//! consumate, verifica che gli eventi arrivino via RPC, e prova la cerimonia di
//! immutabilità (`set-upgrade-authority --final`) accertando che il mercato
//! continui a funzionare dopo che l'authority è stata bruciata.
//!
//! Uso:  ceremony <percorso-keypair-deployer>
//! Presuppone che il programma sia già stato deployato e il validator attivo.

use autonomous_mm_integration::*;
use solana_client::{rpc_client::RpcClient, rpc_config::RpcTransactionConfig};
use solana_system_interface::instruction as system_instruction;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};
use solana_transaction_status_client_types::UiTransactionEncoding;
use solana_commitment_config::CommitmentConfig;
use std::{thread::sleep, time::Duration};

const RPC: &str = "http://127.0.0.1:8899";

fn read_keypair(path: &str) -> Keypair {
    let bytes: Vec<u8> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    Keypair::try_from(&bytes[..]).unwrap()
}

fn airdrop(c: &RpcClient, who: &Pubkey, sol: u64) {
    let sig = c.request_airdrop(who, sol * 1_000_000_000).unwrap();
    for _ in 0..60 {
        if c.confirm_transaction(&sig).unwrap_or(false) {
            return;
        }
        sleep(Duration::from_millis(300));
    }
    panic!("airdrop non confermato");
}

fn send(c: &RpcClient, ixs: &[Instruction], payer: &Keypair, signers: &[&Keypair]) -> Signature {
    let bh = c.get_latest_blockhash().unwrap();
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), signers, bh);
    c.send_and_confirm_transaction(&tx).unwrap()
}

/// Compute unit consumate e log, letti dalla transazione confermata.
fn tx_details(c: &RpcClient, sig: &Signature) -> (u64, Vec<String>) {
    for _ in 0..40 {
        let cfg = RpcTransactionConfig {
            encoding: Some(UiTransactionEncoding::Json),
            commitment: Some(CommitmentConfig::confirmed()),
            max_supported_transaction_version: Some(0),
        };
        if let Ok(tx) = c.get_transaction_with_config(sig, cfg) {
            if let Some(meta) = tx.transaction.meta {
                let cu: u64 = Option::<u64>::from(meta.compute_units_consumed).unwrap_or(0);
                let logs: Vec<String> = Option::<Vec<String>>::from(meta.log_messages).unwrap_or_default();
                return (cu, logs);
            }
        }
        sleep(Duration::from_millis(300));
    }
    panic!("transazione non recuperabile via RPC");
}

/// Crea un token account SPL a mano (create_account + InitializeAccount3),
/// senza dipendere dal programma ATA.
fn create_token_account(c: &RpcClient, payer: &Keypair, owner: &Pubkey) -> Pubkey {
    let acc = Keypair::new();
    let rent = c.get_minimum_balance_for_rent_exemption(165).unwrap();
    let create = system_instruction::create_account(
        &payer.pubkey(),
        &acc.pubkey(),
        rent,
        165,
        &TOKEN_PROGRAM_ID,
    );
    // InitializeAccount3 = istruzione 18, dati: [18] ++ owner
    let mut data = vec![18u8];
    data.extend_from_slice(owner.as_ref());
    let init = Instruction {
        program_id: TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(acc.pubkey(), false),
            AccountMeta::new_readonly(mint_pda(), false),
        ],
        data,
    };
    send(c, &[create, init], payer, &[payer, &acc]);
    acc.pubkey()
}

/// Cerca le righe "Program data:" (gli `emit!` di Anchor) e ne decodifica il
/// payload di TradeEvent.
fn decode_trade_events(logs: &[String]) -> Vec<(bool, u64, u64, u64, u64)> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let mut out = Vec::new();
    for l in logs {
        let Some(b64) = l.strip_prefix("Program data: ") else { continue };
        let Ok(raw) = STANDARD.decode(b64.trim()) else { continue };
        // 8 byte di discriminator + is_buy(1) + 4 x u64 + slot(8)
        if raw.len() < 8 + 1 + 8 * 5 {
            continue;
        }
        let b = &raw[8..];
        let g = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        out.push((b[0] == 1, g(1), g(9), g(17), g(25)));
    }
    out
}

fn upgrade_authority(c: &RpcClient) -> Option<Pubkey> {
    // ProgramData: [4 byte slot][1 byte Option][32 byte authority]
    let (pd, _) = Pubkey::find_program_address(
        &[PROGRAM_ID.as_ref()],
        &solana_sdk_ids::bpf_loader_upgradeable::ID,
    );
    let acc = c.get_account(&pd).ok()?;
    if acc.data.len() < 45 || acc.data[12] != 1 {
        return None;
    }
    Some(Pubkey::try_from(&acc.data[13..45]).ok()?)
}

/// Legge la treasury dallo stato: 8 byte di discriminator, 32 di mint, 32 di treasury.
fn treasury_from_state(c: &RpcClient) -> Pubkey {
    let acc = c.get_account(&state_pda()).expect("stato non trovato: initialize non eseguita?");
    Pubkey::try_from(&acc.data[40..72]).expect("treasury illeggibile")
}

/// Fase successiva a `set-upgrade-authority --final`: il mercato deve
/// continuare a funzionare con l'authority bruciata.
fn post_final(c: &RpcClient, funder: &Keypair) {
    println!("== verifica dopo la finalizzazione ==");
    match upgrade_authority(c) {
        Some(a) => panic!("upgrade authority ancora presente: {a} — la finalizzazione non ha avuto effetto"),
        None => println!("upgrade authority: BRUCIATA"),
    }

    let treasury = treasury_from_state(c);
    let user = Keypair::new();
    airdrop(c, &user.pubkey(), 500);
    let token = create_token_account(c, &user, &user.pubkey());

    let supply_before = {
        let m = c.get_account(&mint_pda()).unwrap();
        u64::from_le_bytes(m.data[36..44].try_into().unwrap())
    };

    let d = 10_000_000u64;
    let expected = curve::cost_buy(supply_before as u128, d as u128) as u64;
    let before = c.get_balance(&user.pubkey()).unwrap();
    let sig = send(c, &[ix_buy(d, u64::MAX, &treasury, &user.pubkey(), &token)], &user, &[&user]);
    let (cu, logs) = tx_details(c, &sig);
    let paid = before - c.get_balance(&user.pubkey()).unwrap() - 5_000;
    println!("buy dopo il final: {paid} lamports, {cu} CU");
    assert_eq!(paid, expected, "il prezzo e' cambiato dopo la finalizzazione");
    assert_eq!(decode_trade_events(&logs).len(), 1, "eventi persi dopo la finalizzazione");

    let before = c.get_balance(&user.pubkey()).unwrap();
    send(c, &[ix_sell(d, 0, &treasury, &user.pubkey(), &token)], &user, &[&user]);
    let got = c.get_balance(&user.pubkey()).unwrap() + 5_000 - before;
    println!("sell dopo il final: {got} lamports");
    assert!(got > 0, "il sell non ha rimborsato");

    let vault = c.get_balance(&vault_pda()).unwrap();
    let rent_floor = c.get_minimum_balance_for_rent_exemption(0).unwrap();
    let supply = {
        let m = c.get_account(&mint_pda()).unwrap();
        u64::from_le_bytes(m.data[36..44].try_into().unwrap())
    };
    let liability = curve::v_up(supply as u128) as u64;
    assert!(vault >= rent_floor + liability, "I5 violato dopo la finalizzazione");
    println!("I5 dopo il final : rispettato");
    let _ = funder;
    println!("\nESITO: il mercato funziona con il codice reso immutabile.");
}

fn main() {
    let path = std::env::args().nth(1).expect("uso: ceremony <keypair-deployer> [post]");
    let deployer = read_keypair(&path);
    let c = RpcClient::new_with_commitment(RPC.to_string(), CommitmentConfig::confirmed());

    if std::env::args().nth(2).as_deref() == Some("post") {
        post_final(&c, &deployer);
        return;
    }

    println!("== stato iniziale ==");
    println!("program id       : {PROGRAM_ID}");
    println!("deployer         : {}", deployer.pubkey());
    match upgrade_authority(&c) {
        Some(a) => println!("upgrade authority: {a}"),
        None => println!("upgrade authority: (nessuna, gia' finale)"),
    }

    let treasury = Keypair::new();
    airdrop(&c, &deployer.pubkey(), 100);
    airdrop(&c, &treasury.pubkey(), 1);

    // --- initialize --------------------------------------------------------
    println!("\n== initialize ==");
    let sig = send(
        &c,
        &[ix_initialize(&deployer.pubkey(), &treasury.pubkey())],
        &deployer,
        &[&deployer, &treasury],
    );
    let (cu_init, _) = tx_details(&c, &sig);
    println!("treasury         : {}", treasury.pubkey());
    println!("compute unit     : {cu_init}");

    // --- utente ------------------------------------------------------------
    let user = Keypair::new();
    airdrop(&c, &user.pubkey(), 500);
    let token = create_token_account(&c, &user, &user.pubkey());

    // --- buy ---------------------------------------------------------------
    println!("\n== buy ==");
    let amount = 50_000_000u64;
    let expected_cost = curve::cost_buy(0, amount as u128) as u64;
    let before = c.get_balance(&user.pubkey()).unwrap();
    let sig = send(
        &c,
        &[ix_buy(amount, u64::MAX, &treasury.pubkey(), &user.pubkey(), &token)],
        &user,
        &[&user],
    );
    let (cu_buy, logs_buy) = tx_details(&c, &sig);
    let after = c.get_balance(&user.pubkey()).unwrap();
    let paid = before - after - 5_000;
    println!("compute unit     : {cu_buy}   (budget di default 200000)");
    println!("costo pagato     : {paid}");
    println!("atteso da curva  : {expected_cost}");
    assert_eq!(paid, expected_cost, "il costo on-chain diverge dalla curva");

    let ev = decode_trade_events(&logs_buy);
    assert_eq!(ev.len(), 1, "atteso un TradeEvent, trovati {}", ev.len());
    let (is_buy, ev_amount, ev_lamports, ev_cut, ev_supply) = ev[0];
    println!("evento           : is_buy={is_buy} amount={ev_amount} lamports={ev_lamports} cut={ev_cut} supply_after={ev_supply}");
    assert!(is_buy, "l'evento non e' un buy");
    assert_eq!(ev_amount, amount, "amount nell'evento errato");
    assert_eq!(ev_lamports, expected_cost, "lamports nell'evento errati");
    assert_eq!(ev_supply, amount, "supply_after nell'evento errata");

    // --- sell --------------------------------------------------------------
    println!("\n== sell ==");
    let half = amount / 2;
    let expected_refund = curve::refund_sell((amount - half) as u128, half as u128) as u64;
    let before = c.get_balance(&user.pubkey()).unwrap();
    let sig = send(
        &c,
        &[ix_sell(half, 0, &treasury.pubkey(), &user.pubkey(), &token)],
        &user,
        &[&user],
    );
    let (cu_sell, logs_sell) = tx_details(&c, &sig);
    let got = c.get_balance(&user.pubkey()).unwrap() + 5_000 - before;
    println!("compute unit     : {cu_sell}   (budget di default 200000)");
    println!("rimborso         : {got}");
    println!("atteso da curva  : {expected_refund}");
    assert_eq!(got, expected_refund, "il rimborso on-chain diverge dalla curva");

    let ev = decode_trade_events(&logs_sell);
    assert_eq!(ev.len(), 1, "atteso un TradeEvent sul sell");
    assert!(!ev[0].0, "l'evento doveva essere un sell");
    assert_eq!(ev[0].4, amount - half, "supply_after nell'evento del sell errata");
    println!("evento           : is_buy={} amount={} lamports={} cut={} supply_after={}",
             ev[0].0, ev[0].1, ev[0].2, ev[0].3, ev[0].4);

    // --- solvibilita' sullo stato reale ------------------------------------
    let vault = c.get_balance(&vault_pda()).unwrap();
    let rent_floor = c.get_minimum_balance_for_rent_exemption(0).unwrap();
    let liability = curve::v_up((amount - half) as u128) as u64;
    println!("\n== solvibilita' ==");
    println!("vault            : {vault}");
    println!("rent floor       : {rent_floor}");
    println!("liability V(S)   : {liability}");
    assert!(vault >= rent_floor + liability, "I5 violato sul cluster reale");
    println!("I5               : rispettato (margine {})", vault - rent_floor - liability);

    println!("\nCU_INIT={cu_init} CU_BUY={cu_buy} CU_SELL={cu_sell}");
    println!("TREASURY={}", treasury.pubkey());
    println!("USER_KEYPAIR_OK token={token}");
}

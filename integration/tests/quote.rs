//! Test metamorfici su `quote`.
//!
//! La proprietà che conta non è "quote restituisce qualcosa", ma che a stato
//! identico il preventivo coincida **al lamport** con il settlement:
//!
//!   quote(Δ).0 == costo effettivo di buy(Δ)
//!   quote(Δ).1 == rimborso effettivo di sell(Δ)
//!
//! Una divergenza anche di un solo lamport farebbe mostrare ai frontend prezzi
//! falsi, quindi ogni confronto è esatto, mai approssimato.

use autonomous_mm_integration::*;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};

const FEE: u64 = 5_000;

/// Porta il mercato alla supply richiesta, lasciando i token a `user`.
fn reach_supply(env: &mut Env, user: &Keypair, token: &Pubkey, target: u64) {
    if target == 0 {
        return;
    }
    let treasury = env.treasury;
    env.send(ix_buy(target, u64::MAX, &treasury, &user.pubkey(), token), &[user])
        .expect("accumulo fino alla supply richiesta");
    assert_eq!(env.supply(), target);
}

/// Costo effettivo di un buy, al netto della fee di transazione.
fn actual_buy_cost(env: &mut Env, user: &Keypair, token: &Pubkey, d: u64) -> u64 {
    let treasury = env.treasury;
    let before = env.lamports(&user.pubkey());
    env.send(ix_buy(d, u64::MAX, &treasury, &user.pubkey(), token), &[user])
        .expect("buy deve riuscire");
    before - env.lamports(&user.pubkey()) - FEE
}

/// Rimborso effettivo di un sell, al netto della fee di transazione.
fn actual_sell_refund(env: &mut Env, user: &Keypair, token: &Pubkey, d: u64) -> u64 {
    let treasury = env.treasury;
    let before = env.lamports(&user.pubkey());
    env.send(ix_sell(d, 0, &treasury, &user.pubkey(), token), &[user])
        .expect("sell deve riuscire");
    env.lamports(&user.pubkey()) + FEE - before
}

// ===========================================================================
// 1. Coerenza preventivo / settlement su una griglia di stati e taglie
// ===========================================================================

#[test]
fn quote_matches_settlement_across_a_grid() {
    // (supply di partenza, taglia del trade)
    let grid: &[(u64, u64)] = &[
        (0, 1),
        (0, 1_000),
        (0, 50_000_000),
        (1_000, 1),
        (10_000_000, 7),
        (10_000_000, 10_000_000),
        (250_000_000_000, 1),
        (250_000_000_000, 1_000_000),
        (250_000_000_000, 250_000_000_000),
        (900_000_000_000, 1),
        (900_000_000_000, 100_000_000),
    ];

    for &(supply, d) in grid {
        let mut env = initialized();
        let (user, token) = env.new_user(900_000);
        reach_supply(&mut env, &user, &token, supply);

        let (q_cost, q_refund) = env.quote(d, &user);

        // --- lato buy: il preventivo deve valere per il buy successivo ------
        let cost = actual_buy_cost(&mut env, &user, &token, d);
        assert_eq!(
            cost, q_cost,
            "supply {supply}, Δ {d}: quote prevedeva {q_cost}, il buy ha addebitato {cost}"
        );
        // ripristina la supply per il confronto sul lato sell
        actual_sell_refund(&mut env, &user, &token, d);
        assert_eq!(env.supply(), supply, "supply non ripristinata");

        // --- lato sell -----------------------------------------------------
        if d <= supply {
            let refund = actual_sell_refund(&mut env, &user, &token, d);
            assert_eq!(
                refund, q_refund,
                "supply {supply}, Δ {d}: quote prevedeva {q_refund}, il sell ha pagato {refund}"
            );
        } else {
            assert_eq!(
                q_refund, 0,
                "supply {supply}, Δ {d}: sopra la supply il rimborso deve essere 0"
            );
        }
    }
}

// ===========================================================================
// 2. Bordi
// ===========================================================================

#[test]
fn quote_returns_zero_refund_when_amount_exceeds_supply() {
    let mut env = initialized();
    let (user, token) = env.new_user(100_000);

    // supply 0: qualunque quantita' supera la supply
    for d in [1u64, 1_000, 10_000_000] {
        let (cost, refund) = env.quote(d, &user);
        assert!(cost > 0, "il costo di acquisto deve essere positivo");
        assert_eq!(refund, 0, "a supply 0 il rimborso deve essere 0 (Δ={d})");
    }

    // con supply positiva, un Δ appena sopra deve comunque dare 0
    reach_supply(&mut env, &user, &token, 1_000_000);
    let (_, refund) = env.quote(1_000_001, &user);
    assert_eq!(refund, 0, "Δ oltre la supply deve dare rimborso 0");

    // esattamente alla supply: rimborso positivo, e coincide col settlement
    let (_, refund_all) = env.quote(1_000_000, &user);
    assert!(refund_all > 0, "vendere l'intera supply deve rimborsare");
    let actual = actual_sell_refund(&mut env, &user, &token, 1_000_000);
    assert_eq!(actual, refund_all, "quote e settlement divergono sull'uscita totale");
    assert_eq!(env.supply(), 0);
}

#[test]
fn quote_is_deterministic_and_state_free() {
    let mut env = initialized();
    let (user, token) = env.new_user(100_000);
    reach_supply(&mut env, &user, &token, 40_000_000);

    let first = env.quote(1_000_000, &user);
    let second = env.quote(1_000_000, &user);
    let third = env.quote(1_000_000, &user);
    assert_eq!(first, second, "quote non deterministica");
    assert_eq!(second, third, "quote non deterministica");

    // quote non deve muovere nulla
    assert_eq!(env.supply(), 40_000_000, "quote ha alterato la supply");
}

#[test]
fn quote_is_monotone_in_amount() {
    let mut env = initialized();
    let (user, token) = env.new_user(500_000);
    reach_supply(&mut env, &user, &token, 100_000_000);

    let mut last_cost = 0u64;
    for d in [1u64, 100, 10_000, 1_000_000, 50_000_000] {
        let (cost, _) = env.quote(d, &user);
        assert!(cost >= last_cost, "il costo deve crescere con la quantita' (Δ={d})");
        last_cost = cost;
    }
}

/// Il preventivo di acquisto cresce con la supply: due stati diversi, stesso Δ.
#[test]
fn quote_price_rises_with_supply() {
    let d = 10_000_000u64;

    let mut low = initialized();
    let (u1, t1) = low.new_user(500_000);
    reach_supply(&mut low, &u1, &t1, 10_000_000);
    let (cost_low, _) = low.quote(d, &u1);

    let mut high = initialized();
    let (u2, t2) = high.new_user(500_000);
    reach_supply(&mut high, &u2, &t2, 800_000_000_000);
    let (cost_high, _) = high.quote(d, &u2);

    assert!(
        cost_high > cost_low,
        "a supply piu' alta lo stesso Δ deve costare di piu': {cost_high} vs {cost_low}"
    );
}

// ===========================================================================
// 3. Divergenza nota fra preventivo e regole di esecuzione
// ===========================================================================

/// `quote` non applica il tetto `MAX_SUPPLY`: preventiva un acquisto che `buy`
/// rifiuterebbe. Non è sfruttabile — nessun fondo si muove — ma un frontend che
/// si fidasse del solo preventivo mostrerebbe un prezzo per un trade impossibile.
/// Il test documenta il comportamento attuale: se un giorno `quote` iniziasse a
/// rifiutare, questo test lo segnalerebbe invece di lasciarlo passare in
/// silenzio.
#[test]
fn quote_prices_trades_that_buy_would_reject() {
    let mut env = initialized();
    let (user, token) = env.new_user(900_000);
    reach_supply(&mut env, &user, &token, 900_000_000_000);

    let over = 200_000_000_000u64; // 900e9 + 200e9 > MAX_SUPPLY
    let (cost, _) = env.quote(over, &user);
    assert!(cost > 0, "quote preventiva comunque il trade");

    let treasury = env.treasury;
    let err = env
        .send(ix_buy(over, u64::MAX, &treasury, &user.pubkey(), &token), &[&user])
        .expect_err("il buy corrispondente deve essere rifiutato");
    assert!(err.contains("Custom(6000)"), "atteso MaxSupplyExceeded, ottenuto {err}");
}

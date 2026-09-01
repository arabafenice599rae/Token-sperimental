//! Suite avversariale sulla matematica pura.
//!
//! Le proprietà sono verificate contro la *specifica*, non contro una seconda
//! implementazione: dove il codice arrotonda, il test controlla la disuguaglianza
//! che definisce l'arrotondamento (`a*DEN >= dn*P0 > (a-1)*DEN` per il ceil),
//! così un errore nella formula non può nascondersi dietro lo stesso errore
//! ripetuto nel test.

use super::*;
use proptest::prelude::*;

const MAXS: u128 = MAX_SUPPLY as u128;
/// Rent floor di un account da 0 byte sul cluster reale.
const FLOOR: u128 = 890_880;

fn n_exact(s: u128) -> u128 {
    3 * S0 * S0 * s + 3 * S0 * s * s + s * s * s
}


// --- strategie: generano coppie valide per costruzione, senza scarti --------

/// (supply, delta) con supply + delta <= MAX_SUPPLY.
fn buy_pair() -> impl Strategy<Value = (u128, u128)> {
    (0u128..MAXS).prop_flat_map(|s| (Just(s), 1u128..=(MAXS - s)))
}

/// (supply, delta) con delta piccolo, lontano dal tetto.
fn buy_pair_small() -> impl Strategy<Value = (u128, u128)> {
    (0u128..(MAXS - 1_000_000), 1u128..=1_000_000u128)
}

/// (supply, delta) con delta <= supply: vendita sempre eseguibile.
fn sell_pair() -> impl Strategy<Value = (u128, u128)> {
    (1u128..=MAXS).prop_flat_map(|s| (Just(s), 1u128..=s))
}

// ---------------------------------------------------------------------------
// 1. Correttezza dell'arrotondamento rispetto alla specifica
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// area_up e' il piu' piccolo intero >= dn*P0/DEN.
    #[test]
    fn area_up_is_exactly_ceil((s, d) in buy_pair()) {
        let a = area_up(s, d).unwrap();
        let num = (n_exact(s + d) - n_exact(s)) * P0_NUM;
        prop_assert!(a * DEN >= num, "area_up sotto il valore esatto");
        if a > 0 {
            prop_assert!((a - 1) * DEN < num, "area_up non e' minimale");
        }
    }

    /// area_down e' il piu' grande intero <= dn*P0/DEN.
    #[test]
    fn area_down_is_exactly_floor((s, d) in buy_pair()) {
        let a = area_down(s, d).unwrap();
        let num = (n_exact(s + d) - n_exact(s)) * P0_NUM;
        prop_assert!(a * DEN <= num, "area_down sopra il valore esatto");
        prop_assert!((a + 1) * DEN > num, "area_down non e' massimale");
    }

    /// Il ceil non e' mai piu' di 1 sopra il floor.
    #[test]
    fn up_and_down_differ_by_at_most_one((s, d) in buy_pair()) {
        let (u, l) = (area_up(s, d).unwrap(), area_down(s, d).unwrap());
        prop_assert!(u >= l && u - l <= 1);
    }
}

// ---------------------------------------------------------------------------
// 2. Nessuna estrazione di valore
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// L1: comprare e rivendere subito la stessa quantita' non e' mai
    /// profittevole, per nessun (s, d).
    #[test]
    fn round_trip_never_profits((s, d) in buy_pair()) {
        let cost = cost_buy(s, d).unwrap().0;
        let back = refund_sell(s, d).unwrap().0;
        prop_assert!(cost >= back, "round trip profittevole: {cost} < {back}");
    }

    /// Spezzare un acquisto non lo rende mai piu' economico (il ceil per pezzo
    /// e' superadditivo).
    #[test]
    fn splitting_a_buy_never_saves((s, d) in buy_pair_small(), k in 2u128..8u128) {
        prop_assume!(d >= 2);
        let bulk = cost_buy(s, d).unwrap().0;
        let chunk = d / k;
        prop_assume!(chunk > 0);
        let mut cur = s;
        let mut total = 0u128;
        for i in 0..k {
            let piece = if i == k - 1 { d - chunk * (k - 1) } else { chunk };
            total += cost_buy(cur, piece).unwrap().0;
            cur += piece;
        }
        prop_assert!(total >= bulk, "spezzare l'acquisto costa meno: {total} < {bulk}");
    }

    /// Spezzare una vendita non rende mai di piu' (il floor per pezzo e'
    /// subadditivo).
    #[test]
    fn splitting_a_sell_never_gains((s, d) in sell_pair(), k in 2u128..8u128) {
        prop_assume!(d >= 2);
        let bulk = refund_sell(s - d, d).unwrap().0;
        let chunk = d / k;
        prop_assume!(chunk > 0);
        let mut cur = s;
        let mut total = 0u128;
        for i in 0..k {
            let piece = if i == k - 1 { d - chunk * (k - 1) } else { chunk };
            total += refund_sell(cur - piece, piece).unwrap().0;
            cur -= piece;
        }
        prop_assert!(total <= bulk, "spezzare la vendita rende di piu': {total} > {bulk}");
    }

    /// Un ciclo chiuso (si torna alla supply di partenza per qualunque
    /// cammino) non puo' produrre profitto per l'utente.
    #[test]
    fn closed_cycle_never_profits(
        s0 in 0u128..500_000_000_000u128,
        steps in prop::collection::vec((any::<bool>(), 1u128..5_000_000u128), 1..12),
    ) {
        let mut s = s0;
        let mut paid = 0u128;    // lamports usciti dall'utente
        let mut got = 0u128;     // lamports entrati all'utente
        let mut path = Vec::new();
        for (is_buy, d) in steps {
            if is_buy && s + d <= MAXS {
                paid += cost_buy(s, d).unwrap().0;
                s += d;
                path.push((true, d));
            } else if !is_buy && d <= s {
                got += refund_sell(s - d, d).unwrap().0;
                s -= d;
                path.push((false, d));
            }
        }
        // riporta la supply al punto di partenza
        if s > s0 {
            got += refund_sell(s0, s - s0).unwrap().0;
        } else if s < s0 {
            paid += cost_buy(s, s0 - s).unwrap().0;
        }
        prop_assert!(paid >= got, "ciclo chiuso profittevole: pagato {paid}, incassato {got}, path {path:?}");
    }
}

// ---------------------------------------------------------------------------
// 3. Solvibilita' (I5, I8) — il cuore della garanzia di liquidita'
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1500))]

    /// Lemma chiave da cui discende I8: il rimborso di una vendita non supera
    /// mai il calo della liability sovrastimata. Senza questo, un SELL potrebbe
    /// portare il vault sotto il rent floor.
    #[test]
    fn refund_never_exceeds_liability_drop((s_post, d) in buy_pair()) {
        let refund = refund_sell(s_post, d).unwrap().0;
        let drop = v_up(s_post + d).unwrap() - v_up(s_post).unwrap();
        prop_assert!(refund <= drop, "rimborso {refund} > calo liability {drop}");
    }

    /// L2 + L3: il cut non supera mai lo spread, e dopo il cut il vault
    /// soddisfa ancora I5.
    #[test]
    fn cut_respects_both_caps(
        spread in 0u128..1_000_000_000u128,
        extra in 0u128..1_000_000_000u128,
        s_post in 0u128..=MAXS,
    ) {
        let required = FLOOR + v_up(s_post).unwrap();
        let vault_pre_cut = required + extra;
        let cut = treasury_cut(spread, vault_pre_cut, FLOOR, s_post).unwrap();
        prop_assert!(cut <= spread, "L2 violato");
        prop_assert!(vault_pre_cut - cut >= required, "L3/I5 violato dopo il cut");
    }

    /// Se il vault e' gia' esattamente al minimo, il cut deve essere zero:
    /// la treasury non puo' mai intaccare la liquidita' dei venditori.
    #[test]
    fn cut_is_zero_at_the_floor(spread in 0u128..1_000_000_000u128, s_post in 0u128..=MAXS) {
        let exact = FLOOR + v_up(s_post).unwrap();
        prop_assert_eq!(treasury_cut(spread, exact, FLOOR, s_post).unwrap(), 0);
    }
}

/// Simulazione completa: sequenza pseudo-casuale lunga di trade con cut attivo,
/// verificando I5 dopo ogni singola operazione e la contabilita' globale.
#[test]
fn long_run_solvency_and_accounting() {
    let (mut s, mut vault, mut treasury) = (0u128, FLOOR, 0u128);
    let (mut collected_spread, mut user_paid, mut user_got) = (0u128, 0u128, 0u128);
    let mut x: u64 = 0xDEADBEEFCAFEBABE;

    for _ in 0..200_000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let d = (x % 5_000_000 + 1) as u128;

        if x & 1 == 0 && s + d <= MAXS {
            let (c, sp) = cost_buy(s, d).unwrap();
            let pre = vault + c;
            let cut = treasury_cut(sp, pre, FLOOR, s + d).unwrap();
            vault = pre - cut;
            treasury += cut;
            collected_spread += sp;
            user_paid += c;
            s += d;
        } else if s >= d {
            let (r, sp) = refund_sell(s - d, d).unwrap();
            assert!(vault >= r + FLOOR, "I8: vault {vault} < refund {r} + floor");
            let pre = vault - r;
            let cut = treasury_cut(sp, pre, FLOOR, s - d).unwrap();
            vault = pre - cut;
            treasury += cut;
            collected_spread += sp;
            user_got += r;
            s -= d;
        }
        assert!(vault >= FLOOR + v_up(s).unwrap(), "I5 violato a supply {s}");
    }

    assert!(treasury <= collected_spread, "la treasury ha preso piu' dello spread");
    assert!(treasury > 0, "la treasury non ha mai incassato");
    // Conservazione: quanto e' uscito dagli utenti finanzia vault e treasury.
    assert_eq!(user_paid - user_got, (vault - FLOOR) + treasury);
}

/// Bank run: dopo una fase di accumulo arbitraria, TUTTI riescono a uscire.
#[test]
fn bank_run_everyone_can_exit() {
    for seed in [1u64, 42, 7777, 0xABCDEF] {
        let (mut s, mut vault, mut treasury) = (0u128, FLOOR, 0u128);
        let mut x = seed | 1;

        // accumulo
        for _ in 0..500 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let d = (x % 1_000_000_000 + 1) as u128;
            if s + d > MAXS {
                continue;
            }
            let (c, sp) = cost_buy(s, d).unwrap();
            let pre = vault + c;
            let cut = treasury_cut(sp, pre, FLOOR, s + d).unwrap();
            vault = pre - cut;
            treasury += cut;
            s += d;
        }

        // uscita totale, a pezzi
        while s > 0 {
            let d = (s / 7).max(1).min(s);
            let (r, sp) = refund_sell(s - d, d).unwrap();
            assert!(vault >= r + FLOOR, "bank run: vault esaurito a supply {s}");
            let pre = vault - r;
            let cut = treasury_cut(sp, pre, FLOOR, s - d).unwrap();
            vault = pre - cut;
            treasury += cut;
            s -= d;
        }

        assert_eq!(s, 0);
        assert!(vault >= FLOOR, "vault sotto il rent floor dopo l'uscita totale");
    }
}

// ---------------------------------------------------------------------------
// 4. Monotonia e coerenza dei prezzi
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Il prezzo marginale non decresce mai con la supply: comprare la stessa
    /// quantita' piu' in alto sulla curva costa almeno altrettanto.
    #[test]
    fn cost_is_monotone_in_supply(s in 0u128..400_000_000_000u128, ds in 1u128..400_000_000_000u128, d in 1u128..1_000_000u128) {
        let low = cost_buy(s, d).unwrap().0;
        let high = cost_buy(s + ds, d).unwrap().0;
        prop_assert!(high >= low, "prezzo non monotono: {high} < {low}");
    }

    /// Comprare di piu' non costa mai di meno.
    #[test]
    fn cost_is_monotone_in_amount(s in 0u128..500_000_000_000u128, d in 1u128..1_000_000u128, extra in 1u128..1_000_000u128) {
        prop_assert!(cost_buy(s, d + extra).unwrap().0 >= cost_buy(s, d).unwrap().0);
    }

    /// v_up e' non decrescente.
    #[test]
    fn liability_is_monotone(a in 0u128..=MAXS, b in 0u128..=MAXS) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        prop_assert!(v_up(hi).unwrap() >= v_up(lo).unwrap());
    }

    /// Lo spread incassato e' sempre >= 0 e coerente col costo.
    #[test]
    fn spread_is_consistent((s, d) in buy_pair_small()) {
        let (cost, spread) = cost_buy(s, d).unwrap();
        let area = area_up(s, d).unwrap();
        prop_assert_eq!(cost - spread, area, "cost - spread != area");
        prop_assert!(cost >= area, "I3: il buy incassa meno dell'area");

        let (refund, held) = refund_sell(s, d).unwrap();
        let area_d = area_down(s, d).unwrap();
        prop_assert_eq!(refund + held, area_d, "refund + trattenuto != area");
        prop_assert!(refund <= area_d, "I4: il sell paga piu' dell'area");
    }
}

// ---------------------------------------------------------------------------
// 5. Bordi e overflow (I7)
// ---------------------------------------------------------------------------

#[test]
fn boundaries_do_not_panic_and_stay_consistent() {
    // supply 0 e supply massima
    assert_eq!(v_up(0).unwrap(), 0);
    assert!(v_up(MAXS).unwrap() > 0);
    assert!(cost_buy(0, 1).unwrap().0 >= 1);
    assert!(cost_buy(MAXS - 1, 1).is_some());
    assert!(refund_sell(MAXS - 1, 1).is_some());
    // l'intera curva in un colpo solo
    assert!(cost_buy(0, MAXS).is_some());
    assert!(refund_sell(0, MAXS).is_some());
    // vendere l'intera supply dalla cima
    let full = refund_sell(0, MAXS).unwrap().0;
    let raise = cost_buy(0, MAXS).unwrap().0;
    assert!(raise > full, "comprare tutto deve costare piu' di rivendere tutto");
}

#[test]
fn overflow_returns_none_instead_of_panicking() {
    // Oltre MAX_SUPPLY le istruzioni rifiutano prima di arrivare qui, ma
    // `quote` accetta qualunque u64: deve restituire None, non esplodere.
    assert!(cost_buy(0, u64::MAX as u128).is_none());
    assert!(cost_buy(MAXS, u64::MAX as u128).is_none());
    assert!(area_up(u64::MAX as u128, u64::MAX as u128).is_none());
    assert!(n_of(u128::MAX).is_none());
}

#[test]
fn worst_case_fits_in_u128_with_known_margin() {
    let worst = n_of(MAXS).unwrap().checked_mul(P0_NUM).unwrap();
    assert!(worst <= u128::MAX);
    // margine dichiarato ~1.12x: se qualcuno alza P0_NUM il test cade subito
    let margin_x1000 = (u128::MAX / (worst / 1000)) as u64;
    assert!(
        (1100..=1200).contains(&margin_x1000),
        "margine di overflow cambiato: {margin_x1000}/1000"
    );
}

#[test]
fn curve_multiplier_is_ten() {
    // P(MAX)/P(0) = (1 + MAX/S0)^2 = 10, verificato in aritmetica intera
    let a = S0 + MAXS;
    let lhs = a * a * 1000;
    let rhs = S0 * S0;
    assert!(lhs >= 9990 * rhs && lhs <= 10010 * rhs, "moltiplicatore != 10x");
}

// ---------------------------------------------------------------------------
// 6. guard_treasury (I11) — esaustivo sui casi al contorno
// ---------------------------------------------------------------------------

#[test]
fn guard_treasury_exhaustive_boundaries() {
    let r: u64 = 890_880;
    for &cut in &[0u64, 1, 100, r - 1, r, r + 1, u64::MAX] {
        for &owner_sys in &[true, false] {
            for &data in &[0usize, 1, 8] {
                for &bal in &[0u64, 1, r - 1, r, r + 1, u64::MAX] {
                    let out = guard_treasury(cut, owner_sys, data, bal, r);
                    // Non puo' mai inventare lamports.
                    assert!(out == 0 || out == cut, "output diverso da 0 o cut");
                    if !owner_sys || data != 0 {
                        assert_eq!(out, 0, "treasury non system-owned o con dati");
                    } else {
                        match bal.checked_add(cut) {
                            Some(t) if t >= r => assert_eq!(out, cut),
                            _ => assert_eq!(out, 0, "overflow o sotto rent"),
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn guard_treasury_never_overflows_balance() {
    // saldo prossimo al massimo: il cut va scartato, non wrappato
    assert_eq!(guard_treasury(u64::MAX, true, 0, u64::MAX, 890_880), 0);
    assert_eq!(guard_treasury(2, true, 0, u64::MAX - 1, 890_880), 0);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    /// Il guard non puo' mai far fallire un trade: restituisce sempre un valore
    /// <= cut, quindi il vault ha sempre i lamports per pagarlo.
    #[test]
    fn guard_output_never_exceeds_cut(
        cut in any::<u64>(), owner in any::<bool>(),
        data in 0usize..64, bal in any::<u64>(), rent in 0u64..2_000_000,
    ) {
        let out = guard_treasury(cut, owner, data, bal, rent);
        prop_assert!(out <= cut);
        prop_assert!(out == 0 || out == cut);
    }
}

// ---------------------------------------------------------------------------
// 7. La commissione e' davvero applicata, e nella misura giusta
// ---------------------------------------------------------------------------
// Senza questi test una mutazione che azzera lo spread di un lato sopravvive:
// il round-trip resterebbe comunque in perdita grazie all'altro lato, e le
// identita' contabili continuerebbero a tornare.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// cost = ceil(area * (1 + φ/2)), verificato contro la specifica.
    #[test]
    fn buy_charges_exactly_half_the_spread((s, d) in buy_pair_small()) {
        let area = area_up(s, d).unwrap();
        let cost = cost_buy(s, d).unwrap().0;
        let target = area * (HALF_DEN + PHI_BPS);
        prop_assert!(cost * HALF_DEN >= target, "buy sotto la commissione dovuta");
        prop_assert!((cost - 1) * HALF_DEN < target, "buy sopra la commissione dovuta");
    }

    /// refund = floor(area * (1 − φ/2)), verificato contro la specifica.
    #[test]
    fn sell_withholds_exactly_half_the_spread((s, d) in buy_pair_small()) {
        let area = area_down(s, d).unwrap();
        let refund = refund_sell(s, d).unwrap().0;
        let target = area * (HALF_DEN - PHI_BPS);
        prop_assert!(refund * HALF_DEN <= target, "sell paga piu' del dovuto");
        prop_assert!((refund + 1) * HALF_DEN > target, "sell paga meno del dovuto");
    }

    /// Su volumi non banali entrambi i lati devono trattenere qualcosa: un lato
    /// a commissione zero e' un errore, non un arrotondamento.
    #[test]
    fn both_sides_actually_collect((s, d) in (0u128..900_000_000_000u128, 1_000_000u128..10_000_000u128)) {
        let (_, buy_spread) = cost_buy(s, d).unwrap();
        let (_, sell_held) = refund_sell(s, d).unwrap();
        prop_assert!(buy_spread > 0, "il lato acquisto non trattiene nulla");
        prop_assert!(sell_held > 0, "il lato vendita non trattiene nulla");
    }

    /// Il costo di un giro completo si avvicina a φ = 1%: fissa la GRANDEZZA
    /// della commissione, non solo il suo segno.
    #[test]
    fn round_trip_costs_about_one_percent(s in 0u128..800_000_000_000u128, d in 10_000_000u128..100_000_000u128) {
        let cost = cost_buy(s, d).unwrap().0;
        let back = refund_sell(s, d).unwrap().0;
        let loss = cost - back;
        // loss/cost deve stare fra 0.9% e 1.1%
        prop_assert!(loss * 1000 >= cost * 9, "giro troppo economico: {loss} su {cost}");
        prop_assert!(loss * 1000 <= cost * 11, "giro troppo caro: {loss} su {cost}");
    }
}

#[test]
fn spread_split_is_symmetric() {
    // A parita' di area, cio' che il buy aggiunge e cio' che il sell trattiene
    // devono coincidere a meno dell'arrotondamento.
    for (s, d) in [(0u128, 50_000_000u128), (300_000_000_000, 10_000_000), (900_000_000_000, 1_000_000)] {
        let (_, buy_spread) = cost_buy(s, d).unwrap();
        let (_, sell_held) = refund_sell(s, d).unwrap();
        let diff = buy_spread.abs_diff(sell_held);
        assert!(
            diff * 1000 <= buy_spread.max(sell_held),
            "spread asimmetrico a (s={s}, d={d}): buy {buy_spread}, sell {sell_held}"
        );
    }
}

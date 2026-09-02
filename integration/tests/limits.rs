//! Limiti estremi, costo di esecuzione, autorità del mint e robustezza agli
//! input malformati — tutto sull'artefatto SBF reale.
//!
//! Sono i quattro percorsi che la campagna precedente non toccava: la curva a
//! scala piena (dove l'aritmetica sfiora il tetto di `u128`), il consumo di
//! compute unit agli estremi anziché in un solo punto, la possibilità che
//! esistano token non pagati, e il comportamento del programma davanti a byte
//! spazzatura.

use autonomous_mm_integration::*;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};
use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

const FEE: u64 = 5_000;

// ===========================================================================
// 1. La curva a scala piena
// ===========================================================================

/// Comprare l'intera curva in una sola istruzione calcola
/// `N(MAX) · P0 ≈ 3,03 × 10³⁸`, l'88% di `u128::MAX`: è esattamente il caso
/// peggiore che il `const assert` dimostra a compile time, e fino a qui non era
/// mai stato **eseguito** sul bytecode reale con `overflow-checks` attivi.
///
/// Poi il mercato viene svuotato fino a supply 0. Se l'aritmetica cede da
/// qualche parte, cede qui.
#[test]
fn full_scale_round_trip_at_max_supply() {
    let mut env = initialized();
    // il costo dell'intera curva è ~475 000 SOL: l'utente va finanziato oltre
    let (whale, token) = env.new_user(600_000);
    let treasury = env.treasury;

    let expected_cost = curve::cost_buy(0, MAX_SUPPLY as u128) as u64;
    let before = env.lamports(&whale.pubkey());

    env.send(
        ix_buy(MAX_SUPPLY, u64::MAX, &treasury, &whale.pubkey(), &token),
        &[&whale],
    )
    .expect("l'acquisto dell'intera curva deve riuscire");

    let paid = before - env.lamports(&whale.pubkey()) - FEE;
    assert_eq!(paid, expected_cost, "costo a scala piena divergente");
    assert_eq!(env.supply(), MAX_SUPPLY, "supply non al massimo");
    assert_eq!(env.token_balance(&token), MAX_SUPPLY);

    // I5 al punto più alto della curva
    let rent_floor = env.svm.minimum_balance_for_rent_exemption(0);
    let liability = curve::v_up(MAX_SUPPLY as u128) as u64;
    assert!(
        env.vault_lamports() >= rent_floor + liability,
        "I5 violato a supply massima"
    );

    // nessun ulteriore acquisto è possibile
    let r = env.send(ix_buy(1, u64::MAX, &treasury, &whale.pubkey(), &token), &[&whale]);
    assert!(r.is_err(), "a supply massima nessun acquisto deve passare");

    // uscita completa, in un colpo solo
    let expected_refund = curve::refund_sell(0, MAX_SUPPLY as u128) as u64;
    let before = env.lamports(&whale.pubkey());
    env.send(
        ix_sell(MAX_SUPPLY, 0, &treasury, &whale.pubkey(), &token),
        &[&whale],
    )
    .expect("l'uscita totale dalla cima della curva deve riuscire");

    let got = env.lamports(&whale.pubkey()) + FEE - before;
    assert_eq!(got, expected_refund, "rimborso a scala piena divergente");
    assert_eq!(env.supply(), 0, "supply non azzerata");
    assert!(env.vault_lamports() >= rent_floor, "vault sotto il rent floor");
    assert!(paid > got, "il giro a scala piena deve restare in perdita");
}

/// La stessa scala, ma raggiunta a passi: verifica che l'accumulo incrementale
/// arrivi allo stesso stato del colpo unico, e che I5 regga a ogni passo.
#[test]
fn incremental_climb_to_max_supply_holds_i5() {
    let mut env = initialized();
    let (whale, token) = env.new_user(600_000);
    let treasury = env.treasury;
    let rent_floor = env.svm.minimum_balance_for_rent_exemption(0);

    let step = MAX_SUPPLY / 8;
    let mut reached = 0u64;
    for i in 0..8u64 {
        let d = if i == 7 { MAX_SUPPLY - reached } else { step };
        env.send(ix_buy(d, u64::MAX, &treasury, &whale.pubkey(), &token), &[&whale])
            .unwrap_or_else(|e| panic!("passo {i} fallito: {e}"));
        reached += d;

        let liability = curve::v_up(reached as u128) as u64;
        assert!(
            env.vault_lamports() >= rent_floor + liability,
            "I5 violato al passo {i} (supply {reached})"
        );
    }
    assert_eq!(env.supply(), MAX_SUPPLY);

    // discesa completa
    let mut left = MAX_SUPPLY;
    while left > 0 {
        let d = (left / 5).max(1).min(left);
        env.send(ix_sell(d, 0, &treasury, &whale.pubkey(), &token), &[&whale])
            .expect("ogni passo di uscita deve riuscire");
        left -= d;
        let liability = curve::v_up(left as u128) as u64;
        assert!(
            env.vault_lamports() >= rent_floor + liability,
            "I5 violato in discesa a supply {left}"
        );
    }
    assert_eq!(env.supply(), 0);
}

// ===========================================================================
// 2. Compute unit agli estremi
// ===========================================================================

/// Il costo in CU va misurato dove l'aritmetica è più pesante, non in un punto
/// comodo: `u128` non costa uguale su tutti i valori. Qui si campionano i
/// quattro angoli (supply bassa/alta × Δ minimo/massimo) e si pretende che il
/// **massimo** resti largamente sotto il budget di default.
#[test]
fn compute_units_stay_far_below_the_budget() {
    const DEFAULT_BUDGET: u64 = 200_000;
    // Soglia di guardia: se una modifica raddoppiasse il costo, il test cade
    // prima che lo scopra un utente con il budget di default.
    const GUARD: u64 = 60_000;

    let mut worst = 0u64;
    let mut report: Vec<(String, u64)> = Vec::new();

    for &(supply, d) in &[
        (0u64, 1u64),
        (0, MAX_SUPPLY),
        (999_999_999_999, 1),
        (500_000_000_000, 500_000_000_000),
    ] {
        let mut env = initialized();
        let (user, token) = env.new_user(600_000);
        let treasury = env.treasury;
        if supply > 0 {
            env.send(ix_buy(supply, u64::MAX, &treasury, &user.pubkey(), &token), &[&user])
                .expect("preparazione dello stato");
        }

        let meta = env
            .send_meta(ix_buy(d, u64::MAX, &treasury, &user.pubkey(), &token), &[&user])
            .expect("buy di misura");
        let cu_buy = meta.compute_units_consumed;

        let meta = env
            .send_meta(ix_sell(d, 0, &treasury, &user.pubkey(), &token), &[&user])
            .expect("sell di misura");
        let cu_sell = meta.compute_units_consumed;

        report.push((format!("supply {supply:>13} Δ {d:>13} buy"), cu_buy));
        report.push((format!("supply {supply:>13} Δ {d:>13} sell"), cu_sell));
        worst = worst.max(cu_buy).max(cu_sell);
    }

    // quote, che nessuno aveva mai misurato
    {
        let mut env = initialized();
        let (user, token) = env.new_user(600_000);
        let treasury = env.treasury;
        env.send(ix_buy(900_000_000_000, u64::MAX, &treasury, &user.pubkey(), &token), &[&user])
            .unwrap();
        let meta = env.send_meta(ix_quote(1_000_000), &[&user]).expect("quote");
        report.push(("quote a supply alta".into(), meta.compute_units_consumed));
        worst = worst.max(meta.compute_units_consumed);
    }

    for (label, cu) in &report {
        println!("{label}: {cu} CU");
    }
    println!("massimo osservato: {worst} CU su {DEFAULT_BUDGET} di budget");

    assert!(
        worst < GUARD,
        "consumo salito a {worst} CU, oltre la soglia di guardia {GUARD} (budget {DEFAULT_BUDGET})"
    );
}

// ===========================================================================
// 3. Nessun token può esistere fuori dalla curva
// ===========================================================================

/// Se qualcuno potesse mintare fuori dal programma, potrebbe rivendere token
/// mai pagati e svuotare il vault **senza violare alcun invariante interno**.
/// È la prima domanda di un audit, e finora non aveva risposta eseguibile.
#[test]
fn nobody_can_mint_or_freeze_outside_the_program() {
    let mut env = initialized();
    let (attacker, token) = env.new_user(1_000);

    // --- le autorità sono quelle attese ---------------------------------
    let mint = env.svm.get_account(&mint_pda()).expect("mint assente");
    // layout SPL Mint: COption<Pubkey> authority (4+32), supply (8),
    // decimals (1), is_initialized (1), COption<Pubkey> freeze (4+32)
    assert_eq!(&mint.data[0..4], &[1, 0, 0, 0], "mint authority assente");
    let mint_authority = Pubkey::try_from(&mint.data[4..36]).unwrap();
    assert_eq!(mint_authority, state_pda(), "mint authority non e' la PDA state");
    assert_eq!(mint.data[44], 6, "decimali diversi da 6");
    assert_eq!(&mint.data[46..50], &[1, 0, 0, 0], "freeze authority assente");
    let freeze_authority = Pubkey::try_from(&mint.data[50..82]).unwrap();
    assert_eq!(freeze_authority, state_pda(), "freeze authority non e' la PDA state");

    // --- MintTo diretto firmato dall'attaccante: deve fallire ------------
    // SPL Token istruzione 7 = MintTo, dati: [7] ++ amount u64
    let mut data = vec![7u8];
    data.extend_from_slice(&1_000_000u64.to_le_bytes());
    let mint_to = Instruction {
        program_id: TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(mint_pda(), false),
            AccountMeta::new(token, false),
            AccountMeta::new_readonly(attacker.pubkey(), true),
        ],
        data,
    };
    env.send(mint_to, &[&attacker])
        .expect_err("un mint esterno deve essere rifiutato");
    assert_eq!(env.supply(), 0, "la supply non deve essere cambiata");
    assert_eq!(env.token_balance(&token), 0, "nessun token deve essere apparso");

    // --- MintTo dichiarando la PDA come authority: nessuno può firmarla --
    // La firma della PDA e' semplicemente assente: si firma solo con
    // l'attaccante e si verifica che la transazione venga rifiutata.
    let mut data = vec![7u8];
    data.extend_from_slice(&1_000_000u64.to_le_bytes());
    let forged = Instruction {
        program_id: TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(mint_pda(), false),
            AccountMeta::new(token, false),
            AccountMeta::new_readonly(state_pda(), true), // richiede firma della PDA
        ],
        data,
    };
    let mut tx = Transaction::new_with_payer(&[forged], Some(&attacker.pubkey()));
    let bh = env.svm.latest_blockhash();
    tx.partial_sign(&[&attacker], bh);
    env.svm
        .send_transaction(tx)
        .expect_err("nessuno puo' firmare per la PDA state");
    assert_eq!(env.supply(), 0);

    // --- FreezeAccount: istruzione 10, stessa logica ---------------------
    let freeze = Instruction {
        program_id: TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(token, false),
            AccountMeta::new_readonly(mint_pda(), false),
            AccountMeta::new_readonly(attacker.pubkey(), true),
        ],
        data: vec![10u8],
    };
    env.send(freeze, &[&attacker])
        .expect_err("un congelamento esterno deve essere rifiutato");

    // --- e il mercato continua a funzionare normalmente ------------------
    let treasury = env.treasury;
    env.send(ix_buy(1_000_000, u64::MAX, &treasury, &attacker.pubkey(), &token), &[&attacker])
        .expect("il mercato deve restare operativo");
    assert_eq!(env.supply(), 1_000_000);
}

// ===========================================================================
// 4. Robustezza agli input malformati
// ===========================================================================

/// Byte spazzatura devono produrre errori puliti, **mai un panic**. La
/// distinzione conta: `ProgramFailedToComplete` significa che il programma è
/// abortito, ed è la firma esatta del difetto `is_on_curve` trovato in questa
/// campagna. Un rifiuto ordinato invece è comportamento corretto.
#[test]
fn malformed_instruction_data_never_panics() {
    let mut env = initialized();
    let (user, token) = env.new_user(10_000);
    let treasury = env.treasury;
    env.send(ix_buy(5_000_000, u64::MAX, &treasury, &user.pubkey(), &token), &[&user])
        .expect("stato di partenza");

    let accounts = vec![
        AccountMeta::new_readonly(state_pda(), false),
        AccountMeta::new(mint_pda(), false),
        AccountMeta::new(vault_pda(), false),
        AccountMeta::new(treasury, false),
        AccountMeta::new(user.pubkey(), true),
        AccountMeta::new(token, false),
        AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
    ];

    let mut cases: Vec<Vec<u8>> = vec![
        vec![],                       // vuoto
        vec![0u8],                    // un byte
        vec![0xFF; 7],                // meno di un discriminator
        vec![0xFF; 8],                // discriminator inesistente
        ix_discriminator("buy").to_vec(),               // buy senza argomenti
        ix_discriminator("sell").to_vec(),              // sell senza argomenti
        ix_discriminator("quote").to_vec(),             // quote senza argomenti
        [ix_discriminator("buy").to_vec(), vec![0u8; 4]].concat(),  // argomenti troncati
        [ix_discriminator("sell").to_vec(), vec![0xFF; 9]].concat(), // lunghezza dispari
        [ix_discriminator("initialize").to_vec(), vec![0xAB; 64]].concat(),
        [ix_discriminator("buy").to_vec(), vec![0xFF; 1024]].concat(), // troppo lunghi
    ];

    // Argomenti BEN FORMATI ma degeneri: senza questi il corpus non entra mai
    // nel corpo del programma, perche' Anchor rifiuta i payload malformati in
    // deserializzazione. Sono questi a esercitare davvero la logica.
    for disc in ["buy", "sell", "quote"] {
        for (a, b) in [
            (0u64, 0u64),
            (0, u64::MAX),
            (u64::MAX, 0),
            (u64::MAX, u64::MAX),
            (MAX_SUPPLY, 0),
            (MAX_SUPPLY + 1, u64::MAX),
            (1, 0),
        ] {
            let mut blob = ix_discriminator(disc).to_vec();
            blob.extend_from_slice(&a.to_le_bytes());
            blob.extend_from_slice(&b.to_le_bytes());
            cases.push(blob);
        }
    }

    // più un lotto pseudo-casuale, deterministico
    let mut x: u64 = 0x5DEECE66D;
    for _ in 0..60 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let len = (x % 40) as usize;
        let mut blob = Vec::with_capacity(len);
        let mut y = x;
        for _ in 0..len {
            y = y.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            blob.push((y >> 33) as u8);
        }
        cases.push(blob);
    }

    let rent_floor = env.svm.minimum_balance_for_rent_exemption(0);
    let mut executed = 0usize;

    for (i, data) in cases.iter().enumerate() {
        let ix = Instruction {
            program_id: PROGRAM_ID,
            accounts: accounts.clone(),
            data: data.clone(),
        };
        match env.send(ix, &[&user]) {
            Err(e) => assert!(
                !e.contains("ProgramFailedToComplete"),
                "caso {i} ({} byte) ha fatto abortire il programma: {e}",
                data.len()
            ),
            // Alcuni payload ben formati sono istruzioni legittime e vengono
            // eseguite: e' corretto che accada, purche' lo stato resti sano.
            Ok(()) => executed += 1,
        }

        let liability = curve::v_up(env.supply() as u128) as u64;
        assert!(
            env.vault_lamports() >= rent_floor + liability,
            "caso {i}: I5 violato dopo un input malformato"
        );
        assert!(env.supply() <= MAX_SUPPLY, "caso {i}: supply oltre il tetto");
    }

    // il corpus deve contenere sia payload rifiutati sia istruzioni valide:
    // se nessuna entrasse nel corpo del programma, il test non proverebbe nulla
    assert!(executed > 0, "nessun payload ha raggiunto la logica del programma");
    assert!(executed < cases.len(), "nessun payload e' stato rifiutato");
}

// ===========================================================================
// 5. Identità del programma
// ===========================================================================

/// Anchor rifiuta di eseguire se il programma è stato deployato a un indirizzo
/// diverso da quello dichiarato in `declare_id!`. È la protezione contro il
/// disallineamento fra keypair di deploy e sorgente — l'errore operativo che
/// il gate `--features production` rende altrimenti facile commettere.
///
/// Verificato eseguendo: lo stesso `.so` caricato a un indirizzo arbitrario
/// rifiuta la prima istruzione con `DeclaredProgramIdMismatch` (4100).
#[test]
fn the_program_refuses_to_run_at_another_address() {
    use litesvm::LiteSVM;

    let wrong_id = Pubkey::new_from_array([9u8; 32]);
    let mut svm = LiteSVM::new().with_default_programs();
    svm.add_program(wrong_id, &program_so()).expect("caricamento");

    let d = deployer();
    svm.airdrop(&d.pubkey(), 1_000_000_000_000).unwrap();
    let treasury = Keypair::new();

    // PDA derivate dall'indirizzo reale di deploy: la transazione è ben
    // formata sotto ogni altro aspetto.
    let seeds = |s: &[u8]| Pubkey::find_program_address(&[s], &wrong_id).0;
    let ix = Instruction {
        program_id: wrong_id,
        accounts: vec![
            AccountMeta::new(seeds(b"state"), false),
            AccountMeta::new(seeds(b"mint"), false),
            AccountMeta::new(seeds(b"vault"), false),
            AccountMeta::new_readonly(treasury.pubkey(), true),
            AccountMeta::new(d.pubkey(), true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(RENT_SYSVAR, false),
        ],
        data: ix_discriminator("initialize").to_vec(),
    };
    let bh = svm.latest_blockhash();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&d.pubkey()), &[&d, &treasury], bh);

    let err = svm
        .send_transaction(tx)
        .expect_err("un deploy a un indirizzo diverso da declare_id! deve essere rifiutato");
    assert!(
        format!("{:?}", err.err).contains("Custom(4100)"),
        "atteso DeclaredProgramIdMismatch (4100), ottenuto {:?}",
        err.err
    );
}

//! Casi al contorno di completezza: aliasing di account, effetto economico di
//! una donazione, evento e stato prodotti da `initialize`, e simmetria di
//! `quote` sulla quantità nulla.

use autonomous_mm_integration::*;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer, transaction::Transaction};

const FEE: u64 = 5_000;
const E_ZERO_AMOUNT: u32 = 6001;

/// Righe `Program data:` emesse da `emit!`, decodificate in byte grezzi.
fn program_data(logs: &[String]) -> Vec<Vec<u8>> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    logs.iter()
        .filter_map(|l| l.strip_prefix("Program data: "))
        .filter_map(|b| STANDARD.decode(b.trim()).ok())
        .collect()
}

// ===========================================================================
// 1. Aliasing di account
// ===========================================================================

/// `user == treasury`: il proprietario della treasury che opera sul proprio
/// mercato. È una classe di vulnerabilità classica su Solana (lo stesso account
/// passato due volte come mutabile), e va misurata, non assunta benigna.
///
/// Atteso: nel BUY il cut è un trasferimento verso sé stessi, quindi l'esborso
/// netto scende di `cut`; nel SELL l'incasso sale di `cut`. In entrambi i casi
/// il vault riceve/paga esattamente quanto in un trade normale, e I5 regge.
#[test]
fn treasury_trading_on_its_own_market_does_not_break_solvency() {
    let mut env = initialized();
    // il trader È la treasury
    let trader = Keypair::try_from(&env.treasury_kp.to_bytes()[..]).unwrap();
    env.svm.airdrop(&trader.pubkey(), 5_000 * LAMPORTS_PER_SOL).unwrap();
    let token = env.create_token_account(&trader.pubkey());
    let treasury = env.treasury;
    assert_eq!(trader.pubkey(), treasury, "il test richiede user == treasury");

    let rent_floor = env.svm.minimum_balance_for_rent_exemption(0);
    let d = 20_000_000u64;
    let cost = curve::cost_buy(0, d as u128) as u64;

    let before = env.lamports(&trader.pubkey());
    let vault_before = env.vault_lamports();
    env.send(ix_buy(d, u64::MAX, &treasury, &trader.pubkey(), &token), &[&trader])
        .expect("la treasury deve poter comprare");

    let out = before - env.lamports(&trader.pubkey()) - FEE;
    let into_vault = env.vault_lamports() - vault_before;
    // il vault incassa costo − cut; l'esborso netto del trader è lo stesso,
    // perché il cut è tornato a lui
    assert_eq!(out, into_vault, "il cut non e' tornato al trader/treasury");
    assert!(out <= cost, "esborso netto superiore al costo pieno");
    assert_eq!(env.supply(), d);
    assert!(
        env.vault_lamports() >= rent_floor + curve::v_up(d as u128) as u64,
        "I5 violato con user == treasury"
    );

    // e l'uscita funziona
    env.send(ix_sell(d, 0, &treasury, &trader.pubkey(), &token), &[&trader])
        .expect("la treasury deve poter vendere");
    assert_eq!(env.supply(), 0);
    assert!(env.vault_lamports() >= rent_floor, "vault sotto il rent floor");
}

/// Lo stesso token account passato come `user_token` da due istruzioni nella
/// stessa transazione: il saldo deve restare coerente, senza doppi conteggi.
#[test]
fn the_same_token_account_twice_in_one_transaction_stays_consistent() {
    let mut env = initialized();
    let (user, token) = env.new_user(5_000);
    let treasury = env.treasury;
    let d = 5_000_000u64;

    let expected = curve::cost_buy(0, d as u128) as u64 + curve::cost_buy(d as u128, d as u128) as u64;
    let before = env.lamports(&user.pubkey());

    let bh = env.svm.latest_blockhash();
    let tx = Transaction::new_signed_with_payer(
        &[
            ix_buy(d, u64::MAX, &treasury, &user.pubkey(), &token),
            ix_buy(d, u64::MAX, &treasury, &user.pubkey(), &token),
        ],
        Some(&user.pubkey()),
        &[&user],
        bh,
    );
    env.svm.send_transaction(tx).expect("due buy sullo stesso token account");

    assert_eq!(env.token_balance(&token), 2 * d, "saldo token incoerente");
    assert_eq!(env.supply(), 2 * d, "supply incoerente");
    assert_eq!(before - env.lamports(&user.pubkey()) - FEE, expected, "prezzo incoerente");
}

// ===========================================================================
// 2. Effetto economico di una donazione al vault
// ===========================================================================

/// Il vault è un system account: chiunque può versarci lamports. Cosa succeda
/// poi a quei fondi è la domanda interessante, e la risposta non è quella che
/// sembra.
///
/// Il cut è cappato dallo spread del singolo trade (L2), e lo spread di un
/// trade è esattamente ciò che quel trade aggiunge sopra la curva. Il tetto
/// `safe` quindi **non morde mai** in esercizio normale: la treasury incassa il
/// flusso, mai lo stock. Di conseguenza l'eccedenza sopra `rent_floor + V(S)`
/// non cala mai — cresce di 0 o 1 lamport per trade, per puro arrotondamento.
///
/// **Lamports versati al vault sono irrecuperabili.** Non tornano al donatore
/// (nessuna istruzione preleva), non migliorano il prezzo dei venditori (I2:
/// dipende solo dalla supply) e non raggiungono la treasury. Restano lì.
/// Chi invia fondi al vault per errore li ha persi.
#[test]
fn lamports_sent_to_the_vault_are_locked_forever() {
    let mut env = initialized();
    let (user, token) = env.new_user(50_000);
    let treasury = env.treasury;
    let d = 10_000_000u64;
    let rent_floor = env.svm.minimum_balance_for_rent_exemption(0);

    env.send(ix_buy(d * 20, u64::MAX, &treasury, &user.pubkey(), &token), &[&user])
        .expect("stato di partenza");

    let excess = |e: &Env| e.vault_lamports() - rent_floor - curve::v_up(e.supply() as u128) as u64;
    let excess_before = excess(&env);

    // --- la donazione ----------------------------------------------------
    let donor = Keypair::new();
    env.svm.airdrop(&donor.pubkey(), 10 * LAMPORTS_PER_SOL).unwrap();
    let donation = 2 * LAMPORTS_PER_SOL;
    let donor_before = env.lamports(&donor.pubkey());
    let treasury_before = env.lamports(&treasury);

    env.send(
        solana_system_interface::instruction::transfer(&donor.pubkey(), &vault_pda(), donation),
        &[&donor],
    )
    .expect("il vault e' system-owned: i depositi sono permissionless");

    assert_eq!(excess(&env), excess_before + donation, "donazione non contabilizzata");
    assert!(
        env.lamports(&donor.pubkey()) <= donor_before - donation,
        "il donatore deve aver speso i fondi"
    );

    // --- il prezzo di vendita non cambia (I2) ----------------------------
    let supply = env.supply();
    let expected_refund = curve::refund_sell((supply - d) as u128, d as u128) as u64;
    let before = env.lamports(&user.pubkey());
    env.send(ix_sell(d, 0, &treasury, &user.pubkey(), &token), &[&user])
        .expect("sell dopo la donazione");
    assert_eq!(
        env.lamports(&user.pubkey()) + FEE - before,
        expected_refund,
        "la donazione ha alterato il prezzo di vendita"
    );

    // --- la treasury incassa lo spread, non la donazione ------------------
    let gained = env.lamports(&treasury) - treasury_before;
    assert!(gained > 0, "la treasury deve incassare lo spread del trade");
    assert!(
        gained < donation / 100,
        "la treasury ha attinto alla donazione: {gained} su {donation}"
    );

    // --- e l'eccedenza non cala mai, su nessun tipo di trade --------------
    let mut previous = excess(&env);
    assert!(previous >= donation, "la donazione deve essere ancora nel vault");

    let mut x: u64 = 0x2545F4914F6CDD1D;
    for i in 0..24 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let amount = (x % 5_000_000) + 1;
        let held = env.token_balance(&token);
        let r = if x & 1 == 0 {
            env.send(ix_buy(amount, u64::MAX, &treasury, &user.pubkey(), &token), &[&user])
        } else if held > 0 {
            env.send(ix_sell(amount.min(held), 0, &treasury, &user.pubkey(), &token), &[&user])
        } else {
            continue;
        };
        r.unwrap_or_else(|e| panic!("trade {i}: {e}"));

        let now = excess(&env);
        assert!(
            now >= previous,
            "trade {i}: l'eccedenza e' calata da {previous} a {now} — il cut ha attinto allo stock"
        );
        assert!(
            now <= previous + 2,
            "trade {i}: l'eccedenza e' cresciuta di {} lamports, oltre l'arrotondamento",
            now - previous
        );
        previous = now;
        assert!(
            env.vault_lamports() >= rent_floor + curve::v_up(env.supply() as u128) as u64,
            "I5 violato al trade {i}"
        );
    }

    // dopo 24 trade la donazione e' ancora tutta li'
    assert!(
        previous >= donation,
        "dopo 24 trade la donazione non e' piu' nel vault: {previous} < {donation}"
    );
}

// ===========================================================================
// 3. initialize: evento e stato
// ===========================================================================

/// Finora erano verificati solo i `TradeEvent`. Qui si controlla che
/// `initialize` emetta il suo evento con i campi giusti e scriva nello stato
/// esattamente ciò che le istruzioni successive useranno.
#[test]
fn initialize_emits_its_event_and_writes_the_expected_state() {
    let mut env = fresh();
    let deployer = deployer();
    let tkp = Keypair::try_from(&env.treasury_kp.to_bytes()[..]).unwrap();
    let treasury = env.treasury;

    let meta = env
        .send_meta(ix_initialize(&deployer.pubkey(), &treasury), &[&deployer, &tkp])
        .expect("initialize");

    // --- evento ----------------------------------------------------------
    let events = program_data(&meta.logs);
    assert_eq!(events.len(), 1, "atteso un solo evento, trovati {}", events.len());
    let ev = &events[0];
    assert!(ev.len() >= 8 + 32 + 32 + 8, "payload dell'evento troppo corto");
    let ev_mint = Pubkey::try_from(&ev[8..40]).unwrap();
    let ev_treasury = Pubkey::try_from(&ev[40..72]).unwrap();
    assert_eq!(ev_mint, mint_pda(), "mint nell'evento errato");
    assert_eq!(ev_treasury, treasury, "treasury nell'evento errata");

    // --- stato -----------------------------------------------------------
    let state = env.svm.get_account(&state_pda()).expect("stato non creato");
    assert_eq!(state.owner, PROGRAM_ID, "lo stato deve appartenere al programma");
    assert_eq!(state.data.len(), 8 + 32 + 32 + 1 + 1 + 1, "dimensione dello stato inattesa");
    assert_eq!(Pubkey::try_from(&state.data[8..40]).unwrap(), mint_pda(), "mint nello stato");
    assert_eq!(Pubkey::try_from(&state.data[40..72]).unwrap(), treasury, "treasury nello stato");
    assert_eq!(state.data[72], pda(b"vault").1, "vault_bump errato");
    assert_eq!(state.data[73], pda(b"state").1, "state_bump errato");
    assert_eq!(state.data[74], pda(b"mint").1, "mint_bump errato");

    // --- vault seminato al rent floor, system-owned e senza dati ----------
    let vault = env.svm.get_account(&vault_pda()).expect("vault non creato");
    assert_eq!(
        vault.lamports,
        env.svm.minimum_balance_for_rent_exemption(0),
        "il vault deve partire esattamente al rent floor"
    );
    assert!(vault.data.is_empty(), "il vault non deve avere dati");
}

// ===========================================================================
// 4. quote: simmetria sulla quantità nulla (I14)
// ===========================================================================

/// `buy(0)` e `sell(0)` restituiscono `ZeroAmount`; `quote(0)` deve fare
/// altrettanto. Un preventivo per un'operazione che il settlement rifiuta è
/// la stessa asimmetria già chiusa sul tetto di supply.
#[test]
fn quote_rejects_zero_like_the_settlement_does() {
    let mut env = initialized();
    let (user, token) = env.new_user(1_000);
    let treasury = env.treasury;

    let err = env.try_quote(0, &user).expect_err("quote(0) deve fallire");
    assert!(err.contains(&format!("Custom({E_ZERO_AMOUNT})")), "atteso ZeroAmount, ottenuto {err}");

    // il settlement risponde allo stesso modo
    let e_buy = env
        .send(ix_buy(0, u64::MAX, &treasury, &user.pubkey(), &token), &[&user])
        .expect_err("buy(0)");
    assert!(e_buy.contains(&format!("Custom({E_ZERO_AMOUNT})")));

    // e con quantità positiva quote continua a funzionare
    let (cost, _) = env.quote(1, &user);
    assert!(cost > 0, "quote(1) deve restare valida");
}

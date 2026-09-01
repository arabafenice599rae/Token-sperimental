//! Helper condivisi dai test di integrazione.
//!
//! Il programma viene caricato come artefatto SBF compilato: questo crate non
//! linka `autonomous-mm`, quindi discriminator, PDA e layout degli account sono
//! ricostruiti qui a mano. È deliberato — un test che riusa le stesse funzioni
//! del programma non può accorgersi se quelle funzioni sono sbagliate.

use litesvm::LiteSVM;
use sha2::{Digest, Sha256};
use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;
use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::Keypair,
    signer::{keypair::keypair_from_seed, Signer},
    transaction::Transaction,
};

pub const PROGRAM_ID: Pubkey =
    solana_sdk::pubkey!("DxRXCM3egzfmcgBXYAt19xUewgnmrZ575XMwUQr8xQCG");
pub const TOKEN_PROGRAM_ID: Pubkey =
    solana_sdk::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const RENT_SYSVAR: Pubkey =
    solana_sdk::pubkey!("SysvarRent111111111111111111111111111111111");

pub const MAX_SUPPLY: u64 = 1_000_000_000_000;

/// Seed fisso del deployer autorizzato. Deterministico, così la pubkey
/// corrispondente può essere compilata dentro il programma come
/// `EXPECTED_DEPLOYER` e i test possono firmare come lui.
pub const DEPLOYER_SEED: [u8; 32] = [7u8; 32];

pub fn deployer() -> Keypair {
    keypair_from_seed(&DEPLOYER_SEED).expect("seed valido")
}

/// Discriminator Anchor di un'istruzione: primi 8 byte di sha256("global:<nome>").
pub fn ix_discriminator(name: &str) -> [u8; 8] {
    let mut h = Sha256::new();
    h.update(format!("global:{name}").as_bytes());
    let out = h.finalize();
    let mut d = [0u8; 8];
    d.copy_from_slice(&out[..8]);
    d
}

pub fn pda(seed: &[u8]) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[seed], &PROGRAM_ID)
}

pub fn state_pda() -> Pubkey {
    pda(b"state").0
}
pub fn mint_pda() -> Pubkey {
    pda(b"mint").0
}
pub fn vault_pda() -> Pubkey {
    pda(b"vault").0
}

// ---------------------------------------------------------------------------
// Costruzione delle istruzioni
// ---------------------------------------------------------------------------

pub fn ix_initialize(payer: &Pubkey, treasury: &Pubkey) -> Instruction {
    let data = ix_discriminator("initialize").to_vec();
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(state_pda(), false),
            AccountMeta::new(mint_pda(), false),
            AccountMeta::new(vault_pda(), false),
            AccountMeta::new_readonly(*treasury, true),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(RENT_SYSVAR, false),
        ],
        data,
    }
}

fn trade_ix(name: &str, a: u64, b: u64, treasury: &Pubkey, user: &Pubkey, user_token: &Pubkey) -> Instruction {
    let mut data = ix_discriminator(name).to_vec();
    data.extend_from_slice(&a.to_le_bytes());
    data.extend_from_slice(&b.to_le_bytes());
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(state_pda(), false),
            AccountMeta::new(mint_pda(), false),
            AccountMeta::new(vault_pda(), false),
            AccountMeta::new(*treasury, false),
            AccountMeta::new(*user, true),
            AccountMeta::new(*user_token, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
        ],
        data,
    }
}

pub fn ix_buy(amount: u64, max_cost: u64, treasury: &Pubkey, user: &Pubkey, user_token: &Pubkey) -> Instruction {
    trade_ix("buy", amount, max_cost, treasury, user, user_token)
}

pub fn ix_sell(amount: u64, min_refund: u64, treasury: &Pubkey, user: &Pubkey, user_token: &Pubkey) -> Instruction {
    trade_ix("sell", amount, min_refund, treasury, user, user_token)
}

/// `quote` e' sola lettura: nessun signer fra i suoi account, solo il payer
/// della transazione.
pub fn ix_quote(amount: u64) -> Instruction {
    let mut data = ix_discriminator("quote").to_vec();
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(state_pda(), false),
            AccountMeta::new_readonly(mint_pda(), false),
        ],
        data,
    }
}

// ---------------------------------------------------------------------------
// Ambiente
// ---------------------------------------------------------------------------

pub fn program_so() -> Vec<u8> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../target/deploy/autonomous_mm.so");
    std::fs::read(path).unwrap_or_else(|e| {
        panic!("artefatto SBF mancante ({path}): esegui `cargo-build-sbf --arch v3` prima dei test — {e}")
    })
}

pub struct Env {
    pub svm: LiteSVM,
    pub deployer: Keypair,
    pub treasury: Pubkey,
    pub treasury_kp: Keypair,
}

/// VM con il programma caricato e il deployer finanziato. Non inizializza.
pub fn fresh() -> Env {
    let mut svm = LiteSVM::new().with_default_programs();
    svm.add_program(PROGRAM_ID, &program_so())
        .expect("caricamento del programma nella VM fallito");
    let deployer = deployer();
    svm.airdrop(&deployer.pubkey(), 10_000 * LAMPORTS_PER_SOL).unwrap();
    // treasury: wallet ordinario sulla curva, distinto da tutto il resto
    let treasury_kp = Keypair::new();
    let treasury = treasury_kp.pubkey();
    Env { svm, deployer, treasury, treasury_kp }
}

pub const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

/// VM con il mercato già inizializzato dal deployer legittimo.
pub fn initialized() -> Env {
    let mut env = fresh();
    let ix = ix_initialize(&env.deployer.pubkey(), &env.treasury);
    let bh = env.svm.latest_blockhash();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&env.deployer.pubkey()),
        &[&env.deployer, &env.treasury_kp],
        bh,
    );
    env.svm.send_transaction(tx).expect("initialize deve riuscire");
    env
}

impl Env {
    pub fn send(&mut self, ix: Instruction, signers: &[&Keypair]) -> Result<(), String> {
        // Due transazioni identiche hanno la stessa firma e la seconda verrebbe
        // rifiutata come gia' processata: si fa avanzare il blockhash.
        self.svm.expire_blockhash();
        let payer = signers[0].pubkey();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer),
            signers,
            self.svm.latest_blockhash(),
        );
        self.svm
            .send_transaction(tx)
            .map(|_| ())
            .map_err(|e| format!("{:?}", e.err))
    }

    /// Crea un utente finanziato con il suo token account già pronto.
    pub fn new_user(&mut self, sol: u64) -> (Keypair, Pubkey) {
        let user = Keypair::new();
        self.svm.airdrop(&user.pubkey(), sol * LAMPORTS_PER_SOL).unwrap();
        let token_account = self.create_token_account(&user.pubkey());
        (user, token_account)
    }

    /// Token account SPL scritto direttamente nello stato: evita di dipendere
    /// dal programma ATA per costruire i casi di test.
    pub fn create_token_account(&mut self, owner: &Pubkey) -> Pubkey {
        let key = Keypair::new().pubkey();
        let mut data = vec![0u8; spl_token_account_len()];
        let account = SplTokenAccount {
            mint: mint_pda(),
            owner: *owner,
            amount: 0,
            delegate: None,
            state: 1, // Initialized
            is_native: None,
            delegated_amount: 0,
            close_authority: None,
        };
        account.pack_into_slice(&mut data);
        self.svm
            .set_account(
                key,
                Account {
                    lamports: self.svm.minimum_balance_for_rent_exemption(data.len()),
                    data,
                    owner: TOKEN_PROGRAM_ID,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap();
        key
    }

    /// Come `send`, ma restituisce i metadati: return data e compute unit.
    pub fn send_meta(
        &mut self,
        ix: Instruction,
        signers: &[&Keypair],
    ) -> Result<litesvm::types::TransactionMetadata, String> {
        self.svm.expire_blockhash();
        let payer = signers[0].pubkey();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer),
            signers,
            self.svm.latest_blockhash(),
        );
        self.svm.send_transaction(tx).map_err(|e| format!("{:?}", e.err))
    }

    /// Esegue `quote` e decodifica la coppia (costo buy, rimborso sell) dai
    /// return data Anchor: borsh di (u64, u64) = 16 byte little-endian.
    pub fn quote(&mut self, amount: u64, payer: &Keypair) -> (u64, u64) {
        self.try_quote(amount, payer).expect("quote non deve fallire")
    }

    /// Variante fallibile: serve ai test che si aspettano un rifiuto.
    pub fn try_quote(&mut self, amount: u64, payer: &Keypair) -> Result<(u64, u64), String> {
        let meta = self.send_meta(ix_quote(amount), &[payer])?;
        let d = &meta.return_data.data;
        assert_eq!(d.len(), 16, "return data di quote inatteso: {} byte", d.len());
        Ok((
            u64::from_le_bytes(d[0..8].try_into().unwrap()),
            u64::from_le_bytes(d[8..16].try_into().unwrap()),
        ))
    }

    pub fn lamports(&self, k: &Pubkey) -> u64 {
        self.svm.get_account(k).map(|a| a.lamports).unwrap_or(0)
    }

    /// Supply letta dal mint SPL (offset 36, 8 byte little-endian).
    pub fn supply(&self) -> u64 {
        let acc = self.svm.get_account(&mint_pda()).expect("mint assente");
        u64::from_le_bytes(acc.data[36..44].try_into().unwrap())
    }

    /// Saldo di un token account SPL (offset 64, 8 byte little-endian).
    pub fn token_balance(&self, k: &Pubkey) -> u64 {
        let acc = self.svm.get_account(k).expect("token account assente");
        u64::from_le_bytes(acc.data[64..72].try_into().unwrap())
    }

    pub fn vault_lamports(&self) -> u64 {
        self.lamports(&vault_pda())
    }
}

// ---------------------------------------------------------------------------
// Layout SPL Token minimo (165 byte) — scritto a mano per non dipendere da spl-token
// ---------------------------------------------------------------------------

pub fn spl_token_account_len() -> usize {
    165
}

struct SplTokenAccount {
    mint: Pubkey,
    owner: Pubkey,
    amount: u64,
    delegate: Option<Pubkey>,
    state: u8,
    is_native: Option<u64>,
    delegated_amount: u64,
    close_authority: Option<Pubkey>,
}

impl SplTokenAccount {
    fn pack_into_slice(&self, dst: &mut [u8]) {
        dst[0..32].copy_from_slice(self.mint.as_ref());
        dst[32..64].copy_from_slice(self.owner.as_ref());
        dst[64..72].copy_from_slice(&self.amount.to_le_bytes());
        match self.delegate {
            Some(d) => {
                dst[72..76].copy_from_slice(&1u32.to_le_bytes());
                dst[76..108].copy_from_slice(d.as_ref());
            }
            None => dst[72..76].copy_from_slice(&0u32.to_le_bytes()),
        }
        dst[108] = self.state;
        match self.is_native {
            Some(v) => {
                dst[109..113].copy_from_slice(&1u32.to_le_bytes());
                dst[113..121].copy_from_slice(&v.to_le_bytes());
            }
            None => dst[109..113].copy_from_slice(&0u32.to_le_bytes()),
        }
        dst[121..129].copy_from_slice(&self.delegated_amount.to_le_bytes());
        match self.close_authority {
            Some(c) => {
                dst[129..133].copy_from_slice(&1u32.to_le_bytes());
                dst[133..165].copy_from_slice(c.as_ref());
            }
            None => dst[129..133].copy_from_slice(&0u32.to_le_bytes()),
        }
    }
}

// silenzia l'import inutilizzato quando Pack non serve
#[allow(dead_code)]
fn _assert_pack_in_scope<T: Pack>() {}

// ---------------------------------------------------------------------------
// Curva di riferimento — implementazione INDIPENDENTE
// ---------------------------------------------------------------------------
// Riscritta dalla specifica in testa al programma, non copiata dal sorgente:
// serve a confrontare i valori prodotti on-chain con un secondo calcolo.
pub mod curve {
    pub const P0: u128 = 100;
    pub const S0: u128 = 462_475_295_574;
    pub const PHI_BPS: u128 = 100;
    pub const HALF: u128 = 20_000;
    pub const DEN: u128 = 3 * S0 * S0;

    fn n(s: u128) -> u128 {
        3 * S0 * S0 * s + 3 * S0 * s * s + s * s * s
    }
    fn div_ceil(a: u128, b: u128) -> u128 {
        (a + b - 1) / b
    }

    /// Costo di un acquisto di `d` unità a partire da supply `s`.
    pub fn cost_buy(s: u128, d: u128) -> u128 {
        let area = div_ceil((n(s + d) - n(s)) * P0, DEN);
        div_ceil(area * (HALF + PHI_BPS), HALF)
    }
    /// Rimborso della vendita di `d` unità che porta la supply a `s_after`.
    pub fn refund_sell(s_after: u128, d: u128) -> u128 {
        let area = ((n(s_after + d) - n(s_after)) * P0) / DEN;
        area * (HALF - PHI_BPS) / HALF
    }
    /// Liability sovrastimata a supply `s`.
    pub fn v_up(s: u128) -> u128 {
        div_ceil(n(s) * P0, DEN)
    }
}

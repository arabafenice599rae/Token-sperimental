// ============================================================================
// MARKET MAKER AUTONOMO MINIMALE — Solana / Anchor — REV 4 (senza cushion)
// ============================================================================
// Curva:  P(s) = P0 * (1 + s/S0)^2,  M = P(MAX)/P(0) = 10×
// Area:   V(s) = P0 * N(s) / (3*S0^2),  N(s) = 3*S0^2*s + 3*S0*s^2 + s^3
//
// BUY  Δ: paga  ceil( ΔV * (1 + φ/2) )   → minta Δ token
// SELL Δ: riceve floor( ΔV * (1 − φ/2) ) → brucia Δ token
//
// REVENUE DEL PROTOCOLLO (nessun cushion):
//   ad ogni trade, lo spread incassato va alla treasury, con TETTO:
//       cut = min( spread ,  R_post_trade − rent_floor − V(S_post) )
//   cioè la treasury riceve al massimo ciò che sta SOPRA il valore della
//   curva. Il vault contiene sempre almeno V(S): questa e' la garanzia di
//   liquidita' per ogni venditore, e non dipende da nessun buffer.
//
// INVARIANTI:
//  I1  supply ∈ [0, MAX_SUPPLY]
//  I2  il prezzo dipende SOLO dalla supply (mai dal tempo)
//  I3  BUY incassa ≥ area della curva aggiunta
//  I4  SELL paga  ≤ area della curva rimossa × (1 − φ/2)
//  I5  reserve − rent_floor − V_up(S) ≥ 0 SEMPRE, anche dopo il cut:
//      il cut e' cappato per costruzione a ciò che sta sopra V_up(S_post).
//      (V_up = V arrotondata per eccesso ⇒ liability sovrastimata.)
//  I6  UNICI outflow dal vault: refund e cut alla treasury nei SELL, entrambi
//      funzioni pure dello stato del trade. Nei BUY il vault riceve solo
//      (cost − cut) e il cut va direttamente user → treasury: nessun outflow.
//      Nessuna istruzione di withdrawal esiste.
//  I7  overflow ⇒ fail; il caso peggiore è provato a COMPILE TIME (const assert)
//  I8  riserva sotto rent floor ⇒ fail
//  I9  parametri economici e treasury immutabili dopo init
//  I10 settlement atomico: mint/burn + tutti i transfer nella stessa ix
//  I11 il cut alla treasury non può mai far fallire un trade, in NESSUNO
//      stato della treasury: se non è (system-owned ∧ 0 dati ∧ rent-exempt
//      post-cut) il cut è 0 e resta nel vault. Verificato a ogni trade.
//  I12 la treasury è un indirizzo sulla curva ed25519 (non PDA di alcun
//      programma) e distinto da vault/state/mint/programmi. Provato
//      facendole firmare initialize: una PDA non può produrre una firma.
//  I13 solo EXPECTED_DEPLOYER può chiamare initialize: senza questo vincolo
//      chiunque potrebbe anticipare il deployer e fissarsi come treasury.
//  I14 `quote` rispetta gli stessi limiti di `buy`/`sell`: non preventiva mai
//      un trade che il settlement rifiuterebbe. Vista e regolamento coincidono.
//
// ROUNDING DIREZIONALE: BUY ceil, SELL floor, V(S) per il check di
// solvibilità ceil (liability sovrastimata), cut floor.
// ============================================================================

use anchor_lang::prelude::*;
use anchor_lang::system_program;
use anchor_spl::token::{self, Burn, Mint, MintTo, Token, TokenAccount};

declare_id!("DxRXCM3egzfmcgBXYAt19xUewgnmrZ575XMwUQr8xQCG");

// ----------------------------------------------------------------------------
// PARAMETRI HARDCODED (I9)
// ----------------------------------------------------------------------------
pub const P0_NUM: u128 = 100; // lamports per unità minima a supply 0
pub const P0_DEN: u128 = 1;
pub const MAX_SUPPLY: u64 = 1_000_000_000_000; // 10^12 unità = 10^6 token interi
pub const S0: u128 = 462_475_295_574; // = MAX/(√10 − 1) → P(MAX) = 10 × P0
pub const PHI_BPS: u128 = 100; // spread totale 1.00%
pub const HALF_DEN: u128 = 20_000; // x*(20000±PHI_BPS)/20000 = x*(1±φ/2)
pub const BPS: u128 = 10_000;
pub const DEN: u128 = P0_DEN * 3 * S0 * S0; // costante, calcolata a compile time
pub const TOKEN_DECIMALS: u8 = 6;

/// Unico account autorizzato a chiamare `initialize`. Senza questo vincolo
/// chiunque potrebbe invocarla per primo fra il deploy e l'init e fissare la
/// treasury sul proprio indirizzo, in modo permanente (I9).
///
/// ATTENZIONE: questo e' il deployer di TEST, derivato dal seed [7u8; 32]
/// (vedi `integration/src/lib.rs`). PRIMA DEL DEPLOY IN PRODUZIONE va
/// sostituito con la pubkey reale del deployer.
pub const EXPECTED_DEPLOYER: Pubkey = pubkey!("GmaDrppBC7P5ARKV8g3djiwP89vz1jLK23V2GBjuAEGB");

// ---- Teoremi a compile time -------------------------------------------------
const fn n_const(s: u128) -> Option<u128> {
    let t1 = match S0.checked_mul(S0) { Some(x) => x, None => return None };
    let t1 = match t1.checked_mul(3) { Some(x) => x, None => return None };
    let t1 = match t1.checked_mul(s) { Some(x) => x, None => return None };
    let t2 = match S0.checked_mul(3) { Some(x) => x, None => return None };
    let ss = match s.checked_mul(s) { Some(x) => x, None => return None };
    let t2 = match t2.checked_mul(ss) { Some(x) => x, None => return None };
    let t3 = match ss.checked_mul(s) { Some(x) => x, None => return None };
    let r = match t1.checked_add(t2) { Some(x) => x, None => return None };
    r.checked_add(t3)
}
// I7: il prodotto peggiore N(MAX)·P0_NUM entra in u128 (margine ~1.12×:
// P0_NUM NON può essere alzato senza ristrutturare area_*; il compilatore
// lo impedirà).
const _: () = assert!(match n_const(MAX_SUPPLY as u128) {
    Some(n) => n.checked_mul(P0_NUM).is_some(),
    None => false,
});
// Coerenza S0 ↔ M=10: 9.99 ≤ (1+MAX/S0)^2 ≤ 10.01
const _: () = assert!({
    let a = S0 + MAX_SUPPLY as u128; // (S0+MAX)^2 / S0^2 = M
    a * a * 1000 >= 9990 * S0 * S0 && a * a * 1000 <= 10010 * S0 * S0
});

#[error_code]
pub enum MmError {
    #[msg("supply massima superata")] MaxSupplyExceeded,
    #[msg("quantita' nulla")] ZeroAmount,
    #[msg("overflow aritmetico")] Overflow,
    #[msg("slippage: costo oltre il massimo accettato")] SlippageBuy,
    #[msg("slippage: rimborso sotto il minimo accettato")] SlippageSell,
    #[msg("riserva insufficiente")] InsufficientReserve,
    #[msg("supply insufficiente per la vendita")] InsufficientSupply,
    #[msg("treasury non valida (PDA, o account del protocollo)")] InvalidTreasury,
    #[msg("chiamante non autorizzato")] Unauthorized,
}

#[event]
pub struct InitializeEvent { pub mint: Pubkey, pub treasury: Pubkey, pub slot: u64 }

#[event]
pub struct TradeEvent {
    pub is_buy: bool,
    pub amount: u64,
    pub lamports: u64,     // pagati (buy) / rimborsati (sell) dall'utente
    pub treasury_cut: u64, // quota spread andata alla treasury in questo trade
    pub supply_after: u64,
    pub slot: u64,
}

// ----------------------------------------------------------------------------
// MATEMATICA PURA (I2: nessuno stato, nessun tempo)
// ----------------------------------------------------------------------------
fn n_of(s: u128) -> Option<u128> { n_const(s) }
#[inline] fn ceil_div(a: u128, b: u128) -> Option<u128> { a.checked_add(b - 1)?.checked_div(b) }
#[inline] fn floor_div(a: u128, b: u128) -> Option<u128> { a.checked_div(b) }

/// Area [s, s+Δ] per eccesso (BUY).
fn area_up(s: u128, d: u128) -> Option<u128> {
    let dn = n_of(s.checked_add(d)?)?.checked_sub(n_of(s)?)?;
    ceil_div(dn.checked_mul(P0_NUM)?, DEN)
}
/// Area [s, s+Δ] per difetto (SELL).
fn area_down(s: u128, d: u128) -> Option<u128> {
    let dn = n_of(s.checked_add(d)?)?.checked_sub(n_of(s)?)?;
    floor_div(dn.checked_mul(P0_NUM)?, DEN)
}
/// V(S) per eccesso: usata come LIABILITY nel check di solvibilità (I5).
fn v_up(s: u128) -> Option<u128> { ceil_div(n_of(s)?.checked_mul(P0_NUM)?, DEN) }

/// BUY: (costo totale, di cui spread)  — I3
fn cost_buy(s: u128, d: u128) -> Option<(u128, u128)> {
    let a = area_up(s, d)?;
    let c = ceil_div(a.checked_mul(HALF_DEN.checked_add(PHI_BPS)?)?, HALF_DEN)?;
    Some((c, c.checked_sub(a)?))
}
/// SELL: (rimborso, di cui spread trattenuto) — I4
fn refund_sell(s_after: u128, d: u128) -> Option<(u128, u128)> {
    let a = area_down(s_after, d)?;
    let r = floor_div(a.checked_mul(HALF_DEN.checked_sub(PHI_BPS)?)?, HALF_DEN)?;
    Some((r, a.checked_sub(r)?))
}

/// CUT — funzione pura. Restituisce quanto dello spread va alla treasury,
/// con tetto: DOPO il cut il vault deve soddisfare R − rent_floor ≥ V_up(S_post).
///   vault_pre_cut = lamports nel vault dopo il trade, prima del cut
fn treasury_cut(spread: u128, vault_pre_cut: u128, rent_floor: u128, s_post: u128) -> Option<u128> {
    let required = rent_floor.checked_add(v_up(s_post)?)?;
    let safe = vault_pre_cut.saturating_sub(required);
    Some(spread.min(safe))
}
/// I11: il cut non deve mai far fallire il trade né finire in un account
/// da cui la treasury prevista non può muoverlo. Cut = 0 se la treasury
/// non è system-owned, o ha dati, o non sarebbe rent-exempt dopo il cut.
/// Funzione pura, deterministica.
fn guard_treasury(cut: u64, owner_is_system: bool, data_len: usize,
                  treasury_lamports: u64, rent_min: u64) -> u64 {
    if !owner_is_system || data_len != 0 { return 0; }
    match treasury_lamports.checked_add(cut) {
        Some(total) if total >= rent_min => cut,
        _ => 0, // sotto rent, o overflow del saldo: il cut resta nel vault
    }
}

// LEMMI (audit):
//  L1  cost_buy.0 ≥ refund_sell.0 per ogni (s,Δ)      ⇒ no round-trip profit
//  L2  cut ≤ spread                                ⇒ R − V(S) non decresce
//  L3  cut ≤ vault_pre_cut − rent_floor − V_up      ⇒ I5 post-cut, sempre

// ----------------------------------------------------------------------------
// STATO
// ----------------------------------------------------------------------------
#[account]
pub struct MarketState {
    pub mint: Pubkey,
    pub treasury: Pubkey, // I9: scritta una volta, mai modificabile
    pub vault_bump: u8,
    pub state_bump: u8,
}

// ----------------------------------------------------------------------------
// PROGRAMMA
// ----------------------------------------------------------------------------
#[program]
pub mod autonomous_mm {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        // I13: solo il deployer previsto puo' inizializzare. Chiude la finestra
        // di front-running fra deploy e init.
        require_keys_eq!(
            ctx.accounts.payer.key(),
            EXPECTED_DEPLOYER,
            MmError::Unauthorized
        );
        // I12: la treasury deve FIRMARE. Una firma ed25519 valida prova il
        // possesso della chiave privata, quindi che l'indirizzo sta sulla
        // curva, quindi che non e' la PDA di nessun programma.
        //
        // NOTA: `Pubkey::is_on_curve()` NON e' utilizzabile qui — sotto
        // target_os="solana" il suo corpo e' `unimplemented!()` e farebbe
        // panicare il programma on-chain.
        let treasury = ctx.accounts.treasury.key();
        require!(treasury != ctx.accounts.vault.key(), MmError::InvalidTreasury);
        require!(treasury != ctx.accounts.state.key(), MmError::InvalidTreasury);
        require!(treasury != ctx.accounts.mint.key(), MmError::InvalidTreasury);
        require!(treasury != system_program::ID, MmError::InvalidTreasury);
        require!(treasury != token::ID, MmError::InvalidTreasury);
        require!(treasury != crate::ID, MmError::InvalidTreasury);
        let mint_key = ctx.accounts.mint.key();
        let st = &mut ctx.accounts.state;
        st.mint = mint_key;
        st.treasury = treasury;
        st.vault_bump = ctx.bumps.vault;
        st.state_bump = ctx.bumps.state;
        // Seed rent-exempt del vault (floor permanente, mai prelevabile).
        let seed = Rent::get()?.minimum_balance(0);
        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: ctx.accounts.payer.to_account_info(),
                    to: ctx.accounts.vault.to_account_info(),
                },
            ),
            seed,
        )?;
        emit!(InitializeEvent { mint: mint_key, treasury, slot: Clock::get()?.slot });
        Ok(())
    }

    pub fn buy(ctx: Context<Trade>, amount: u64, max_cost_lamports: u64) -> Result<()> {
        require!(amount > 0, MmError::ZeroAmount);
        let s = ctx.accounts.mint.supply;
        let s_post = s.checked_add(amount).ok_or(MmError::Overflow)?;
        require!(s_post <= MAX_SUPPLY, MmError::MaxSupplyExceeded); // I1

        let (cost, spread) = cost_buy(s as u128, amount as u128).ok_or(MmError::Overflow)?;
        let cost: u64 = cost.try_into().map_err(|_| MmError::Overflow)?;
        require!(cost <= max_cost_lamports, MmError::SlippageBuy);

        // Waterfall sullo stato POST-trade (vault + cost), PRE-cut
        let rent_min = Rent::get()?.minimum_balance(0);
        let rent_floor = Rent::get()?.minimum_balance(ctx.accounts.vault.data_len());
        let vault_pre_cut = (ctx.accounts.vault.lamports() as u128)
            .checked_add(cost as u128).ok_or(MmError::Overflow)?;
        let cut = treasury_cut(spread, vault_pre_cut, rent_floor as u128, s_post as u128)
            .ok_or(MmError::Overflow)?;
        let cut: u64 = cut.try_into().map_err(|_| MmError::Overflow)?;
        let t = &ctx.accounts.treasury;
        let cut = guard_treasury(cut, t.owner == &system_program::ID, t.data_len(),
                                 t.lamports(), rent_min);

        // I10/I6: utente → vault (cost − cut), utente → treasury (cut)
        let sys = ctx.accounts.system_program.to_account_info();
        system_program::transfer(
            CpiContext::new(sys.clone(), system_program::Transfer {
                from: ctx.accounts.user.to_account_info(),
                to: ctx.accounts.vault.to_account_info(),
            }),
            cost - cut,
        )?;
        if cut > 0 {
            system_program::transfer(
                CpiContext::new(sys, system_program::Transfer {
                    from: ctx.accounts.user.to_account_info(),
                    to: ctx.accounts.treasury.to_account_info(),
                }),
                cut,
            )?;
        }
        let seeds: &[&[u8]] = &[b"state", &[ctx.accounts.state.state_bump]];
        token::mint_to(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                MintTo {
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.user_token.to_account_info(),
                    authority: ctx.accounts.state.to_account_info(),
                },
                &[seeds],
            ),
            amount,
        )?;
        emit!(TradeEvent { is_buy: true, amount, lamports: cost, treasury_cut: cut,
                           supply_after: s_post, slot: Clock::get()?.slot });
        Ok(())
    }

    /// I6: unica istruzione con outflow dal vault (refund + cut).
    pub fn sell(ctx: Context<Trade>, amount: u64, min_refund_lamports: u64) -> Result<()> {
        require!(amount > 0, MmError::ZeroAmount);
        let s = ctx.accounts.mint.supply;
        let s_post = s.checked_sub(amount).ok_or(MmError::InsufficientSupply)?; // I1

        let (refund, spread) = refund_sell(s_post as u128, amount as u128).ok_or(MmError::Overflow)?;
        let refund: u64 = refund.try_into().map_err(|_| MmError::Overflow)?;
        require!(refund >= min_refund_lamports, MmError::SlippageSell);

        let rent_min = Rent::get()?.minimum_balance(0);
        let rent_floor = Rent::get()?.minimum_balance(ctx.accounts.vault.data_len());
        let vault_pre_cut = ctx.accounts.vault.lamports()
            .checked_sub(refund).ok_or(MmError::InsufficientReserve)?;
        require!(vault_pre_cut >= rent_floor, MmError::InsufficientReserve); // I8 (mai attivo per I5)

        let cut = treasury_cut(spread, vault_pre_cut as u128, rent_floor as u128, s_post as u128)
            .ok_or(MmError::Overflow)?;
        let cut: u64 = cut.try_into().map_err(|_| MmError::Overflow)?;
        let t = &ctx.accounts.treasury;
        let cut = guard_treasury(cut, t.owner == &system_program::ID, t.data_len(),
                                 t.lamports(), rent_min);

        token::burn(
            CpiContext::new(ctx.accounts.token_program.to_account_info(), Burn {
                mint: ctx.accounts.mint.to_account_info(),
                from: ctx.accounts.user_token.to_account_info(),
                authority: ctx.accounts.user.to_account_info(),
            }),
            amount,
        )?;
        // vault (system-owned, 0 dati) → utente / treasury via CPI firmata
        let vault_seeds: &[&[u8]] = &[b"vault", &[ctx.accounts.state.vault_bump]];
        let sys = ctx.accounts.system_program.to_account_info();
        system_program::transfer(
            CpiContext::new_with_signer(sys.clone(), system_program::Transfer {
                from: ctx.accounts.vault.to_account_info(),
                to: ctx.accounts.user.to_account_info(),
            }, &[vault_seeds]),
            refund,
        )?;
        if cut > 0 {
            system_program::transfer(
                CpiContext::new_with_signer(sys, system_program::Transfer {
                    from: ctx.accounts.vault.to_account_info(),
                    to: ctx.accounts.treasury.to_account_info(),
                }, &[vault_seeds]),
                cut,
            )?;
        }
        emit!(TradeEvent { is_buy: false, amount, lamports: refund, treasury_cut: cut,
                           supply_after: s_post, slot: Clock::get()?.slot });
        Ok(())
    }

    /// Quote read-only: (costo buy, rimborso sell) per `amount`.
    ///
    /// I14: il preventivo rispetta gli stessi limiti del settlement. Preventivare
    /// un acquisto che `buy` rifiuterebbe sarebbe informazione falsa, e i
    /// frontend si fidano del preventivo: e' il suo scopo.
    pub fn quote(ctx: Context<Quote>, amount: u64) -> Result<(u64, u64)> {
        let supply = ctx.accounts.mint.supply;
        let s_post = supply.checked_add(amount).ok_or(MmError::Overflow)?;
        require!(s_post <= MAX_SUPPLY, MmError::MaxSupplyExceeded); // I1, come in buy
        let s = supply as u128;
        let (c, _) = cost_buy(s, amount as u128).ok_or(MmError::Overflow)?;
        let r = if (amount as u128) <= s {
            refund_sell(s - amount as u128, amount as u128).ok_or(MmError::Overflow)?.0
        } else { 0 };
        Ok((c.try_into().map_err(|_| MmError::Overflow)?,
            r.try_into().map_err(|_| MmError::Overflow)?))
    }
}

// ----------------------------------------------------------------------------
// ACCOUNTS
// ----------------------------------------------------------------------------
#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(init, payer = payer, space = 8 + 32 + 32 + 1 + 1, seeds = [b"state"], bump)]
    pub state: Account<'info, MarketState>,
    #[account(init, payer = payer, mint::decimals = TOKEN_DECIMALS,
              mint::authority = state, mint::freeze_authority = state,
              seeds = [b"mint"], bump)]
    pub mint: Account<'info, Mint>,
    /// CHECK: vault = PDA SYSTEM-OWNED, 0 dati. Nessun `init`: il PDA viene
    /// passato alla transazione come account vuoto; e' il transfer di lamports
    /// del seed rent-exempt a farne un system account finanziato. System-owned
    /// e' deliberato: abilita la CPI firmata (invoke_signed) in sell e rende
    /// impossibile la manipolazione diretta dei lamports da parte del programma.
    #[account(mut, seeds = [b"vault"], bump)]
    pub vault: UncheckedAccount<'info>,
    /// La treasury firma l'init: e' l'unico modo on-chain di dimostrare che
    /// l'indirizzo sta sulla curva ed25519 (I12). Non riceve e non paga nulla
    /// in questa istruzione.
    pub treasury: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct Trade<'info> {
    #[account(seeds = [b"state"], bump = state.state_bump)]
    pub state: Account<'info, MarketState>,
    #[account(mut, seeds = [b"mint"], bump, address = state.mint)]
    pub mint: Account<'info, Mint>,
    /// PDA riserva. SystemAccount: verifica owner == System Program. Non puo'
    /// mai fallire in un flusso legittimo (solo il programma puo' firmare per
    /// questa PDA e non chiama mai `assign`), quindi e' una cintura gratuita.
    #[account(mut, seeds = [b"vault"], bump = state.vault_bump)]
    pub vault: SystemAccount<'info>,
    /// CHECK: treasury = indirizzo fissato in state (I9); riceve solo cut.
    /// DELIBERATAMENTE NON `SystemAccount`: quel vincolo farebbe fallire ogni
    /// trade se il proprietario della treasury la riassegnasse a un programma
    /// (kill-switch del mercato). Invece guard_treasury (I11) verifica owner,
    /// dati e rent a ogni trade e in caso anomalo lascia il cut nel vault:
    /// il mercato non si ferma mai, la treasury malconfigurata perde solo revenue.
    #[account(mut, address = state.treasury)]
    pub treasury: UncheckedAccount<'info>,
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, token::mint = mint, token::authority = user)]
    pub user_token: Account<'info, TokenAccount>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Quote<'info> {
    #[account(seeds = [b"state"], bump = state.state_bump)]
    pub state: Account<'info, MarketState>,
    #[account(seeds = [b"mint"], bump, address = state.mint)]
    pub mint: Account<'info, Mint>,
}

#[cfg(test)]
mod tests_math;

// ============================================================================
// TEST DELLE PROPRIETÀ (matematica pura)
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    const FLOOR: u128 = 890_880;

    #[test] // L1
    fn no_round_trip_profit() {
        for s in [0u128, 1, 999, 10_000, 1_000_000, 500_000_000_000] {
            for d in [1u128, 7, 1_000, 999_999, 100_000_000] {
                if s + d > MAX_SUPPLY as u128 { continue; }
                assert!(cost_buy(s, d).unwrap().0 >= refund_sell(s, d).unwrap().0);
            }
        }
    }

    #[test] // I5 post-cut, induttivo, con cut attivo
    fn solvency_with_cut_fuzz() {
        let (mut s, mut vault, mut treasury) = (0u128, FLOOR, 0u128);
        let mut x: u64 = 0x9E3779B97F4A7C15;
        for _ in 0..50_000 {
            x ^= x << 13; x ^= x >> 7; x ^= x << 17;
            let d = (x % 1_000_000 + 1) as u128;
            if x & 1 == 0 && s + d <= MAX_SUPPLY as u128 {
                let (c, sp) = cost_buy(s, d).unwrap();
                let pre = vault + c;
                let cut = treasury_cut(sp, pre, FLOOR, s + d).unwrap();
                assert!(cut <= sp);                                   // L2
                vault = pre - cut; treasury += cut; s += d;
            } else if s >= d {
                let (r, sp) = refund_sell(s - d, d).unwrap();
                assert!(vault >= r + FLOOR, "I8");
                let pre = vault - r;
                let cut = treasury_cut(sp, pre, FLOOR, s - d).unwrap();
                assert!(cut <= sp);
                vault = pre - cut; treasury += cut; s -= d;
            }
            assert!(vault >= FLOOR + v_up(s).unwrap(), "I5 violato");     // sempre, anche post-cut
        }
        assert!(treasury > 0, "la treasury non ha mai ricevuto");
    }

    #[test] // il regime di puro accumulo DEVE pagare la treasury
    fn treasury_paid_in_pure_accumulation() {
        let (mut s, mut vault, mut treasury) = (0u128, FLOOR, 0u128);
        for _ in 0..1000 {
            let d = 100_000_000u128;
            let (c, sp) = cost_buy(s, d).unwrap();
            let pre = vault + c;
            let cut = treasury_cut(sp, pre, FLOOR, s + d).unwrap();
            vault = pre - cut; treasury += cut; s += d;
        }
        assert!(treasury > 0, "treasury non pagata in solo-buy");
    }

    #[test] // I11 — funzione pura; i casi reali vanno su localnet
    fn guard_never_bricks_trade() {
        let r = 890_880;
        assert_eq!(guard_treasury(100, true, 0, 0, r), 0);          // vuota, cut piccolo
        assert_eq!(guard_treasury(100, true, 0, r, r), 100);        // esattamente al rent
        assert_eq!(guard_treasury(100, true, 0, r - 101, r), 0);    // sotto il rent anche col cut
        assert_eq!(guard_treasury(100, true, 0, r - 100, r), 100);  // il cut porta esattamente al rent
        assert_eq!(guard_treasury(100, true, 0, r - 1, r), 100);    // gia' sopra il rent dopo il cut
        assert_eq!(guard_treasury(900_000, true, 0, 0, r), 900_000);// cut sufficiente
        assert_eq!(guard_treasury(900_000, false, 0, 0, r), 0);     // non system-owned
        assert_eq!(guard_treasury(900_000, true, 8, r, r), 0);      // ha dati
    }
}

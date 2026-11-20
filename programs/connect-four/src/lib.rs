//! Connect Four with USDC wagers.
//!
//! Lifecycle:
//! 1. Solana: `create_game` escrows the creator's wager in a vault owned by the `Game` PDA.
//!    `cancel_game` refunds it while nobody has joined.
//! 2. Solana: `join_game` escrows the opponent's wager; `delegate_game` (same transaction)
//!    delegates the `Game` PDA to the ER validator chosen at creation.
//! 3. ER: the creator's `start_game` creates the ephemeral `Board` and two `Player` accounts,
//!    sponsored by the delegated `Game`. `play_move`, `claim_timeout` and `resign` run on the ER.
//!    If the game is never started, `abort_game` refunds both players after `START_TIMEOUT_SECS`.
//! 4. ER: `finish_game` copies the result into `Game`, closes the three ephemeral accounts
//!    (rent goes back to `Game`) and commits + undelegates `Game` to Solana.
//! 5. Solana: `settle` pays the vault out to the winner (or splits on a draw) and closes
//!    the vault and the `Game` account.
#![allow(ambiguous_glob_reexports)]

use anchor_lang::prelude::*;
use anchor_lang::system_program;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{self, CloseAccount, Mint, Token, TokenAccount, TransferChecked};
use ephemeral_rollups_sdk::anchor::{commit, delegate, ephemeral, ephemeral_accounts};
use ephemeral_rollups_sdk::cpi::DelegateConfig;
use ephemeral_rollups_sdk::ephem::MagicIntentBundleBuilder;

pub mod errors;
pub mod state;

use errors::ConnectFourError;
use state::*;

declare_id!("9bji6FPUqL4E6CX7cdNAebFjT8CtFoAybP6jgXoud3GJ");

#[ephemeral]
#[program]
pub mod connect_four {
    use super::*;

    // ───────────────────────────── Solana (base layer) ─────────────────────────────

    pub fn create_game(
        ctx: Context<CreateGame>,
        id: u64,
        wager: u64,
        validator: Pubkey,
    ) -> Result<()> {
        require!(wager > 0, ConnectFourError::InvalidWager);
        require!(
            ALLOWED_VALIDATORS.contains(&validator),
            ConnectFourError::ValidatorNotAllowed
        );

        let game = &mut ctx.accounts.game;
        game.id = id;
        game.player_one = ctx.accounts.player_one.key();
        game.player_two = Pubkey::default();
        game.mint = ctx.accounts.mint.key();
        game.wager = wager;
        game.validator = validator;
        game.status = GameStatus::Open;
        game.result = GameResult::InProgress;
        game.end_reason = EndReason::None;
        game.created_at = Clock::get()?.unix_timestamp;
        game.joined_at = 0;
        game.bump = ctx.bumps.game;

        token::transfer_checked(
            CpiContext::new(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from: ctx.accounts.player_one_token.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.vault.to_account_info(),
                    authority: ctx.accounts.player_one.to_account_info(),
                },
            ),
            wager,
            ctx.accounts.mint.decimals,
        )?;

        // Lamports the game PDA spends as sponsor of the ER-only accounts; refunded on close.
        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.key(),
                system_program::Transfer {
                    from: ctx.accounts.player_one.to_account_info(),
                    to: ctx.accounts.game.to_account_info(),
                },
            ),
            SPONSOR_LAMPORTS,
        )?;
        Ok(())
    }

    pub fn cancel_game(ctx: Context<CancelGame>) -> Result<()> {
        let game = &ctx.accounts.game;
        let id = game.id.to_le_bytes();
        let seeds: &[&[u8]] = &[GAME_SEED, game.player_one.as_ref(), &id, &[game.bump]];

        token::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from: ctx.accounts.vault.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.player_one_token.to_account_info(),
                    authority: ctx.accounts.game.to_account_info(),
                },
                &[seeds],
            ),
            ctx.accounts.vault.amount,
            ctx.accounts.mint.decimals,
        )?;
        token::close_account(CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            CloseAccount {
                account: ctx.accounts.vault.to_account_info(),
                destination: ctx.accounts.player_one.to_account_info(),
                authority: ctx.accounts.game.to_account_info(),
            },
            &[seeds],
        ))?;
        Ok(())
    }

    /// `expected_wager` pins the terms the joiner saw: a creator cannot cancel and re-create
    /// the same game id with a bigger wager between the joiner reading and joining.
    pub fn join_game(ctx: Context<JoinGame>, expected_wager: u64) -> Result<()> {
        let game = &mut ctx.accounts.game;
        require_keys_neq!(
            ctx.accounts.player_two.key(),
            game.player_one,
            ConnectFourError::CannotJoinOwnGame
        );
        require!(
            game.wager == expected_wager,
            ConnectFourError::WagerMismatch
        );
        game.player_two = ctx.accounts.player_two.key();
        game.status = GameStatus::Active;
        game.joined_at = Clock::get()?.unix_timestamp;

        token::transfer_checked(
            CpiContext::new(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from: ctx.accounts.player_two_token.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.vault.to_account_info(),
                    authority: ctx.accounts.player_two.to_account_info(),
                },
            ),
            ctx.accounts.game.wager,
            ctx.accounts.mint.decimals,
        )?;
        Ok(())
    }

    /// Delegates an active game to the validator chosen at creation. Sent in the same
    /// transaction as `join_game`.
    pub fn delegate_game(ctx: Context<DelegateGame>) -> Result<()> {
        require_keys_eq!(
            *ctx.accounts.game.owner,
            crate::ID,
            ErrorCode::AccountOwnedByWrongProgram
        );
        // Scoped borrow: the delegate CPI needs the account data afterwards.
        let game = {
            let data = ctx.accounts.game.try_borrow_data()?;
            Game::try_deserialize(&mut &data[..])?
        };
        require!(
            game.status == GameStatus::Active,
            ConnectFourError::GameNotActive
        );

        let id = game.id.to_le_bytes();
        let seeds: &[&[u8]] = &[GAME_SEED, game.player_one.as_ref(), &id];
        ctx.accounts.delegate_game(
            &ctx.accounts.payer,
            seeds,
            DelegateConfig {
                validator: Some(game.validator),
                ..Default::default()
            },
        )?;
        Ok(())
    }

    /// Pays out the escrow once the finished game has been committed back to Solana.
    /// Permissionless: anyone may trigger settlement; funds only go to the two players.
    pub fn settle(ctx: Context<Settle>) -> Result<()> {
        let game = &ctx.accounts.game;
        let id = game.id.to_le_bytes();
        let seeds: &[&[u8]] = &[GAME_SEED, game.player_one.as_ref(), &id, &[game.bump]];

        let pot = ctx.accounts.vault.amount;
        let (to_player_one, to_player_two) = match game.result {
            GameResult::PlayerOneWins => (pot, 0),
            GameResult::PlayerTwoWins => (0, pot),
            GameResult::Draw => (pot - pot / 2, pot / 2),
            GameResult::InProgress => return err!(ConnectFourError::GameInProgress),
        };

        for (amount, destination) in [
            (to_player_one, &ctx.accounts.player_one_token),
            (to_player_two, &ctx.accounts.player_two_token),
        ] {
            if amount == 0 {
                continue;
            }
            token::transfer_checked(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.key(),
                    TransferChecked {
                        from: ctx.accounts.vault.to_account_info(),
                        mint: ctx.accounts.mint.to_account_info(),
                        to: destination.to_account_info(),
                        authority: ctx.accounts.game.to_account_info(),
                    },
                    &[seeds],
                ),
                amount,
                ctx.accounts.mint.decimals,
            )?;
        }

        token::close_account(CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            CloseAccount {
                account: ctx.accounts.vault.to_account_info(),
                destination: ctx.accounts.player_one.to_account_info(),
                authority: ctx.accounts.game.to_account_info(),
            },
            &[seeds],
        ))?;
        msg!("settled {:?}: p1 +{} p2 +{}", game.result, to_player_one, to_player_two);
        Ok(())
    }

    // ───────────────────────────── Ephemeral Rollup ─────────────────────────────

    /// Creates the ER-only board and player accounts, sponsored by the delegated game PDA.
    /// Only the creator can start: it proves they are present before their first-move clock
    /// runs (otherwise a joiner could start and immediately win on timeout).
    pub fn start_game(ctx: Context<StartGame>) -> Result<()> {
        let game = &ctx.accounts.game;
        require!(
            game.status == GameStatus::Active,
            ConnectFourError::GameNotActive
        );
        require_keys_eq!(
            ctx.accounts.signer.key(),
            game.player_one,
            ConnectFourError::OnlyCreatorCanStart
        );

        ctx.accounts
            .create_ephemeral_board((8 + Board::INIT_SPACE) as u32)?;
        ctx.accounts
            .create_ephemeral_player_one((8 + Player::INIT_SPACE) as u32)?;
        ctx.accounts
            .create_ephemeral_player_two((8 + Player::INIT_SPACE) as u32)?;

        let game_key = game.key();
        let board = Board {
            game: game_key,
            cells: [0; CELLS],
            turn: 1,
            move_count: 0,
            last_column: u8::MAX,
            last_move_ts: Clock::get()?.unix_timestamp,
            result: GameResult::InProgress,
            end_reason: EndReason::None,
            bump: ctx.bumps.board,
        };
        board.try_serialize(&mut &mut ctx.accounts.board.try_borrow_mut_data()?[..])?;

        for (account, wallet, piece, bump) in [
            (&ctx.accounts.player_one, game.player_one, 1, ctx.bumps.player_one),
            (&ctx.accounts.player_two, game.player_two, 2, ctx.bumps.player_two),
        ] {
            let player = Player {
                game: game_key,
                wallet,
                piece,
                moves: 0,
                bump,
            };
            player.try_serialize(&mut &mut account.try_borrow_mut_data()?[..])?;
        }
        Ok(())
    }

    pub fn play_move(ctx: Context<PlayMove>, column: u8) -> Result<()> {
        let board = &mut ctx.accounts.board;
        let player = &mut ctx.accounts.player;
        require!(
            board.result == GameResult::InProgress,
            ConnectFourError::GameOver
        );
        require!(board.turn == player.piece, ConnectFourError::NotYourTurn);

        let (_, connected) = board
            .drop_piece(column as usize, player.piece)
            .ok_or(ConnectFourError::InvalidColumn)?;
        board.move_count += 1;
        board.last_column = column;
        board.last_move_ts = Clock::get()?.unix_timestamp;
        player.moves += 1;

        if connected {
            board.result = if player.piece == 1 {
                GameResult::PlayerOneWins
            } else {
                GameResult::PlayerTwoWins
            };
            board.end_reason = EndReason::ConnectFour;
        } else if board.move_count as usize == CELLS {
            board.result = GameResult::Draw;
            board.end_reason = EndReason::BoardFull;
        } else {
            board.turn = 3 - board.turn;
        }
        Ok(())
    }

    /// The player waiting on an opponent who has not moved for `TURN_TIMEOUT_SECS` wins.
    pub fn claim_timeout(ctx: Context<PlayerAction>) -> Result<()> {
        let board = &mut ctx.accounts.board;
        let player = &ctx.accounts.player;
        require!(
            board.result == GameResult::InProgress,
            ConnectFourError::GameOver
        );
        require!(
            board.turn != player.piece,
            ConnectFourError::CannotClaimOwnTimeout
        );
        let now = Clock::get()?.unix_timestamp;
        require!(
            now.saturating_sub(board.last_move_ts) >= TURN_TIMEOUT_SECS,
            ConnectFourError::TimeoutNotReached
        );
        board.result = if player.piece == 1 {
            GameResult::PlayerOneWins
        } else {
            GameResult::PlayerTwoWins
        };
        board.end_reason = EndReason::Timeout;
        Ok(())
    }

    pub fn resign(ctx: Context<PlayerAction>) -> Result<()> {
        let board = &mut ctx.accounts.board;
        require!(
            board.result == GameResult::InProgress,
            ConnectFourError::GameOver
        );
        board.result = if ctx.accounts.player.piece == 1 {
            GameResult::PlayerTwoWins
        } else {
            GameResult::PlayerOneWins
        };
        board.end_reason = EndReason::Resignation;
        Ok(())
    }

    /// Refunds both players (recorded as an aborted draw) when the game was never started on
    /// the ER within `START_TIMEOUT_SECS` of the join: the creator never showed up, or board
    /// creation was blocked. Commits + undelegates the game so `settle` can pay out.
    pub fn abort_game(ctx: Context<AbortGame>) -> Result<()> {
        let game = &mut ctx.accounts.game;
        require!(
            game.status == GameStatus::Active,
            ConnectFourError::GameNotActive
        );
        let signer = ctx.accounts.payer.key();
        require!(
            signer == game.player_one || signer == game.player_two,
            ConnectFourError::NotAPlayer
        );
        // A started game has a program-owned board; it must be finished through play.
        require!(
            *ctx.accounts.board.owner != crate::ID,
            ConnectFourError::GameAlreadyStarted
        );
        let now = Clock::get()?.unix_timestamp;
        require!(
            now.saturating_sub(game.joined_at) >= START_TIMEOUT_SECS,
            ConnectFourError::StartTimeoutNotReached
        );

        game.status = GameStatus::Finished;
        game.result = GameResult::Draw;
        game.end_reason = EndReason::Aborted;
        game.exit(&crate::ID)?;
        MagicIntentBundleBuilder::new(
            ctx.accounts.payer.to_account_info(),
            ctx.accounts.magic_context.to_account_info(),
            ctx.accounts.magic_program.to_account_info(),
        )
        .commit_and_undelegate(&[ctx.accounts.game.to_account_info()])
        .build_and_invoke()?;
        Ok(())
    }

    /// Records the result on the game PDA, closes the ephemeral accounts and commits +
    /// undelegates the game back to Solana for settlement. Permissionless once the board
    /// has a result.
    pub fn finish_game(ctx: Context<FinishGame>) -> Result<()> {
        let board = {
            let data = ctx.accounts.board.try_borrow_data()?;
            Board::try_deserialize(&mut &data[..])?
        };
        require!(
            board.result != GameResult::InProgress,
            ConnectFourError::GameInProgress
        );

        let game = &mut ctx.accounts.game;
        require!(
            game.status == GameStatus::Active,
            ConnectFourError::GameNotActive
        );
        game.status = GameStatus::Finished;
        game.result = board.result;
        game.end_reason = board.end_reason;

        ctx.accounts.close_ephemeral_board()?;
        ctx.accounts.close_ephemeral_player_one()?;
        ctx.accounts.close_ephemeral_player_two()?;

        ctx.accounts.game.exit(&crate::ID)?;
        MagicIntentBundleBuilder::new(
            ctx.accounts.payer.to_account_info(),
            ctx.accounts.magic_context.to_account_info(),
            ctx.accounts.magic_program.to_account_info(),
        )
        .commit_and_undelegate(&[ctx.accounts.game.to_account_info()])
        .build_and_invoke()?;
        Ok(())
    }
}

// ───────────────────────────── Account contexts ─────────────────────────────

#[derive(Accounts)]
#[instruction(id: u64)]
pub struct CreateGame<'info> {
    #[account(mut)]
    pub player_one: Signer<'info>,
    #[account(
        init,
        payer = player_one,
        space = 8 + Game::INIT_SPACE,
        seeds = [GAME_SEED, player_one.key().as_ref(), &id.to_le_bytes()],
        bump
    )]
    pub game: Account<'info, Game>,
    pub mint: Account<'info, Mint>,
    // init_if_needed: the vault ATA address is public, so plain `init` could be griefed by
    // someone creating it first.
    #[account(
        init_if_needed,
        payer = player_one,
        associated_token::mint = mint,
        associated_token::authority = game,
    )]
    pub vault: Account<'info, TokenAccount>,
    #[account(mut, token::mint = mint, token::authority = player_one)]
    pub player_one_token: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CancelGame<'info> {
    #[account(mut)]
    pub player_one: Signer<'info>,
    #[account(
        mut,
        seeds = [GAME_SEED, game.player_one.as_ref(), &game.id.to_le_bytes()],
        bump = game.bump,
        has_one = player_one,
        has_one = mint,
        constraint = game.status == GameStatus::Open @ ConnectFourError::GameNotOpen,
        close = player_one,
    )]
    pub game: Account<'info, Game>,
    pub mint: Account<'info, Mint>,
    #[account(mut, associated_token::mint = mint, associated_token::authority = game)]
    pub vault: Account<'info, TokenAccount>,
    #[account(mut, token::mint = mint, token::authority = player_one)]
    pub player_one_token: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct JoinGame<'info> {
    #[account(mut)]
    pub player_two: Signer<'info>,
    #[account(
        mut,
        seeds = [GAME_SEED, game.player_one.as_ref(), &game.id.to_le_bytes()],
        bump = game.bump,
        has_one = mint,
        constraint = game.status == GameStatus::Open @ ConnectFourError::GameNotOpen,
    )]
    pub game: Account<'info, Game>,
    pub mint: Account<'info, Mint>,
    #[account(mut, associated_token::mint = mint, associated_token::authority = game)]
    pub vault: Account<'info, TokenAccount>,
    #[account(mut, token::mint = mint, token::authority = player_two)]
    pub player_two_token: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

#[delegate]
#[derive(Accounts)]
pub struct DelegateGame<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: Owner, discriminator and status are checked in the handler; the delegate CPI
    /// re-derives the PDA from the seeds we pass.
    #[account(mut, del)]
    pub game: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct Settle<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        mut,
        seeds = [GAME_SEED, game.player_one.as_ref(), &game.id.to_le_bytes()],
        bump = game.bump,
        has_one = player_one,
        has_one = player_two,
        has_one = mint,
        constraint = game.status == GameStatus::Finished @ ConnectFourError::GameNotFinished,
        close = player_one,
    )]
    pub game: Account<'info, Game>,
    /// CHECK: Receives vault and game rent (the creator paid it); bound by `has_one`.
    #[account(mut)]
    pub player_one: UncheckedAccount<'info>,
    /// CHECK: Bound by `has_one`; only used as the ATA authority.
    pub player_two: UncheckedAccount<'info>,
    pub mint: Account<'info, Mint>,
    #[account(mut, associated_token::mint = mint, associated_token::authority = game)]
    pub vault: Account<'info, TokenAccount>,
    #[account(
        init_if_needed,
        payer = payer,
        associated_token::mint = mint,
        associated_token::authority = player_one,
    )]
    pub player_one_token: Account<'info, TokenAccount>,
    #[account(
        init_if_needed,
        payer = payer,
        associated_token::mint = mint,
        associated_token::authority = player_two,
    )]
    pub player_two_token: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[ephemeral_accounts]
#[derive(Accounts)]
pub struct StartGame<'info> {
    pub signer: Signer<'info>,
    #[account(
        mut,
        sponsor,
        seeds = [GAME_SEED, game.player_one.as_ref(), &game.id.to_le_bytes()],
        bump = game.bump,
    )]
    pub game: Account<'info, Game>,
    /// CHECK: ER-only board, created here.
    #[account(mut, eph, seeds = [BOARD_SEED, game.key().as_ref()], bump)]
    pub board: UncheckedAccount<'info>,
    /// CHECK: ER-only player record, created here.
    #[account(mut, eph, seeds = [PLAYER_SEED, game.key().as_ref(), game.player_one.as_ref()], bump)]
    pub player_one: UncheckedAccount<'info>,
    /// CHECK: ER-only player record, created here.
    #[account(mut, eph, seeds = [PLAYER_SEED, game.key().as_ref(), game.player_two.as_ref()], bump)]
    pub player_two: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct PlayMove<'info> {
    pub signer: Signer<'info>,
    #[account(
        seeds = [GAME_SEED, game.player_one.as_ref(), &game.id.to_le_bytes()],
        bump = game.bump,
    )]
    pub game: Account<'info, Game>,
    #[account(mut, seeds = [BOARD_SEED, game.key().as_ref()], bump = board.bump, has_one = game)]
    pub board: Account<'info, Board>,
    /// The signer's player record: it only exists for the two players of this game.
    #[account(
        mut,
        seeds = [PLAYER_SEED, game.key().as_ref(), signer.key().as_ref()],
        bump = player.bump,
        has_one = game,
    )]
    pub player: Account<'info, Player>,
}

#[derive(Accounts)]
pub struct PlayerAction<'info> {
    pub signer: Signer<'info>,
    #[account(
        seeds = [GAME_SEED, game.player_one.as_ref(), &game.id.to_le_bytes()],
        bump = game.bump,
    )]
    pub game: Account<'info, Game>,
    #[account(mut, seeds = [BOARD_SEED, game.key().as_ref()], bump = board.bump, has_one = game)]
    pub board: Account<'info, Board>,
    #[account(
        seeds = [PLAYER_SEED, game.key().as_ref(), signer.key().as_ref()],
        bump = player.bump,
        has_one = game,
    )]
    pub player: Account<'info, Player>,
}

#[commit]
#[derive(Accounts)]
pub struct AbortGame<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        mut,
        seeds = [GAME_SEED, game.player_one.as_ref(), &game.id.to_le_bytes()],
        bump = game.bump,
    )]
    pub game: Account<'info, Game>,
    /// CHECK: Only its owner is read, to prove the game was never started.
    #[account(seeds = [BOARD_SEED, game.key().as_ref()], bump)]
    pub board: UncheckedAccount<'info>,
}

#[ephemeral_accounts]
#[commit]
#[derive(Accounts)]
pub struct FinishGame<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        mut,
        sponsor,
        seeds = [GAME_SEED, game.player_one.as_ref(), &game.id.to_le_bytes()],
        bump = game.bump,
    )]
    pub game: Account<'info, Game>,
    /// CHECK: ER-only board, deserialized in the handler and closed here.
    #[account(mut, eph, seeds = [BOARD_SEED, game.key().as_ref()], bump)]
    pub board: UncheckedAccount<'info>,
    /// CHECK: ER-only player record, closed here.
    #[account(mut, eph, seeds = [PLAYER_SEED, game.key().as_ref(), game.player_one.as_ref()], bump)]
    pub player_one: UncheckedAccount<'info>,
    /// CHECK: ER-only player record, closed here.
    #[account(mut, eph, seeds = [PLAYER_SEED, game.key().as_ref(), game.player_two.as_ref()], bump)]
    pub player_two: UncheckedAccount<'info>,
}

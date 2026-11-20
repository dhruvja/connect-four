//! MagicSVM integration tests for the connect-four program.
//!
//! Every test drives the real SBF binary (`anchor build` → target/deploy/connect_four.so)
//! through both ledgers: escrow + settlement on base, gameplay on the ephemeral rollup.
//!
//! Run: `cd tests-svm && cargo test`

use {
    borsh::BorshDeserialize,
    ephemeral_rollups_sdk::{
        consts::{EPHEMERAL_VAULT_ID, MAGIC_CONTEXT_ID, MAGIC_PROGRAM_ID},
        delegate_args::DelegateAccounts,
    },
    magicsvm::{MagicSVM, TransactionTarget},
    sha2::{Digest, Sha256},
    solana_account::Account,
    solana_address::{address, Address},
    solana_clock::Clock,
    solana_instruction::{error::InstructionError, AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_message::Message,
    solana_program_pack::Pack,
    solana_rent::Rent,
    solana_sdk_ids::system_program,
    solana_signer::Signer,
    solana_transaction::Transaction,
    solana_transaction_error::TransactionError,
    spl_associated_token_account_interface::address::get_associated_token_address,
    spl_token_interface::state::{Account as TokenAccount, AccountState, Mint},
};

const PROGRAM_ID: Address = address!("9bji6FPUqL4E6CX7cdNAebFjT8CtFoAybP6jgXoud3GJ");
const TOKEN_PROGRAM_ID: Address = spl_token_interface::ID;
const ATA_PROGRAM_ID: Address = spl_associated_token_account_interface::program::ID;
const DELEGATION_PROGRAM_ID: Address = address!("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");

const WAGER: u64 = 5_000_000; // 5 USDC
const START_USDC: u64 = 100_000_000; // 100 USDC
const SPONSOR_LAMPORTS: u64 = 100_000;
const TURN_TIMEOUT_SECS: i64 = 60;
const START_TIMEOUT_SECS: i64 = 120;
/// Devnet Asia ER validator: games must be delegated to an allow-listed validator.
const VALIDATOR: Address = address!("MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57");

// Program error codes (6000 + index in errors.rs) and Anchor framework codes.
const INVALID_WAGER: u32 = 6000;
const GAME_NOT_OPEN: u32 = 6001;
const GAME_NOT_ACTIVE: u32 = 6002;
const CANNOT_JOIN_OWN_GAME: u32 = 6004;
const NOT_A_PLAYER: u32 = 6005;
const GAME_OVER: u32 = 6006;
const GAME_IN_PROGRESS: u32 = 6007;
const NOT_YOUR_TURN: u32 = 6008;
const INVALID_COLUMN: u32 = 6009;
const TIMEOUT_NOT_REACHED: u32 = 6010;
const CANNOT_CLAIM_OWN_TIMEOUT: u32 = 6011;
const VALIDATOR_NOT_ALLOWED: u32 = 6012;
const WAGER_MISMATCH: u32 = 6013;
const ONLY_CREATOR_CAN_START: u32 = 6014;
const GAME_ALREADY_STARTED: u32 = 6015;
const START_TIMEOUT_NOT_REACHED: u32 = 6016;
const ANCHOR_CONSTRAINT_HAS_ONE: u32 = 2001;
const ANCHOR_CONSTRAINT_SEEDS: u32 = 2006;
const ANCHOR_ACCOUNT_NOT_INITIALIZED: u32 = 3012;
const ANCHOR_ACCOUNT_OWNED_BY_WRONG_PROGRAM: u32 = 3007;

const BASE: TransactionTarget = TransactionTarget::Base;
const ER: TransactionTarget = TransactionTarget::Ephemeral;

/// 42 moves (player one first) that fill the board without four in a row.
const DRAW_SEQUENCE: [u8; 42] = [
    3, 3, 3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 4, 4, 4, 4, 4, 4, 0, 1, 1, 1, 1, 1, 1, 5, 5, 5, 5, 5, 5,
    0, 0, 0, 0, 0, 6, 6, 6, 6, 6, 6,
];

// ───────────────────────────── On-chain layouts ─────────────────────────────

#[derive(BorshDeserialize, Debug, PartialEq, Clone, Copy)]
enum GameStatus {
    Open,
    Active,
    Finished,
}

#[derive(BorshDeserialize, Debug, PartialEq, Clone, Copy)]
enum GameResult {
    InProgress,
    PlayerOneWins,
    PlayerTwoWins,
    Draw,
}

#[derive(BorshDeserialize, Debug, PartialEq, Clone, Copy)]
enum EndReason {
    None,
    ConnectFour,
    BoardFull,
    Timeout,
    Resignation,
    Aborted,
}

#[derive(BorshDeserialize, Debug)]
#[allow(dead_code)]
struct Game {
    id: u64,
    player_one: Address,
    player_two: Address,
    mint: Address,
    wager: u64,
    validator: Address,
    status: GameStatus,
    result: GameResult,
    end_reason: EndReason,
    created_at: i64,
    joined_at: i64,
    bump: u8,
}

#[derive(BorshDeserialize, Debug)]
#[allow(dead_code)]
struct Board {
    game: Address,
    cells: [u8; 42],
    turn: u8,
    move_count: u8,
    last_column: u8,
    last_move_ts: i64,
    result: GameResult,
    end_reason: EndReason,
    bump: u8,
}

#[derive(BorshDeserialize, Debug)]
#[allow(dead_code)]
struct Player {
    game: Address,
    wallet: Address,
    piece: u8,
    moves: u8,
    bump: u8,
}

fn decode<T: BorshDeserialize>(account: &Account, name: &str) -> T {
    assert_eq!(&account.data[..8], &discriminator("account", name), "{name} discriminator");
    T::deserialize(&mut &account.data[8..]).unwrap()
}

fn discriminator(namespace: &str, name: &str) -> [u8; 8] {
    let hash = Sha256::digest(format!("{namespace}:{name}").as_bytes());
    hash[..8].try_into().unwrap()
}

fn ix_data(name: &str, args: &[u8]) -> Vec<u8> {
    let mut data = discriminator("global", name).to_vec();
    data.extend_from_slice(args);
    data
}

// ───────────────────────────── Harness ─────────────────────────────

struct Env {
    svm: MagicSVM,
    mint: Address,
    p1: Keypair,
    p2: Keypair,
    stranger: Keypair,
    game_id: u64,
}

type TxResult = Result<(), (TransactionError, Vec<String>)>;

impl Env {
    fn new() -> Self {
        let mut svm = MagicSVM::new();
        let so = concat!(env!("CARGO_MANIFEST_DIR"), "/../target/deploy/connect_four.so");
        let bytes = std::fs::read(so).unwrap_or_else(|e| panic!("{so}: {e} (run `anchor build`)"));
        svm.add_program(PROGRAM_ID, &bytes).unwrap();

        let mint = Address::new_unique();
        let mut mint_data = vec![0; Mint::LEN];
        Mint {
            mint_authority: Some(Address::new_unique()).into(),
            supply: 3 * START_USDC,
            decimals: 6,
            is_initialized: true,
            freeze_authority: None.into(),
        }
        .pack_into_slice(&mut mint_data);
        set_program_account(&mut svm, mint, TOKEN_PROGRAM_ID, mint_data);

        let env = Self {
            svm,
            mint,
            p1: Keypair::new(),
            p2: Keypair::new(),
            stranger: Keypair::new(),
            game_id: 42,
        };
        let mut env = env;
        for wallet in [env.p1.pubkey(), env.p2.pubkey(), env.stranger.pubkey()] {
            env.svm.airdrop(&wallet, 10_000_000_000).unwrap();
            env.set_token_account(wallet, START_USDC);
        }
        env
    }

    fn set_token_account(&mut self, owner: Address, amount: u64) {
        let mut data = vec![0; TokenAccount::LEN];
        TokenAccount {
            mint: self.mint,
            owner,
            amount,
            delegate: None.into(),
            state: AccountState::Initialized,
            is_native: None.into(),
            delegated_amount: 0,
            close_authority: None.into(),
        }
        .pack_into_slice(&mut data);
        let ata = get_associated_token_address(&owner, &self.mint);
        set_program_account(&mut self.svm, ata, TOKEN_PROGRAM_ID, data);
    }

    fn send(&mut self, target: TransactionTarget, signers: &[&Keypair], ixs: &[Instruction]) -> TxResult {
        let tx = Transaction::new(
            signers,
            Message::new(ixs, Some(&signers[0].pubkey())),
            self.svm.latest_blockhash_for(target),
        );
        let result = self.svm.send_transaction_to(target, tx);
        // Fresh blockhash so an identical follow-up transaction is not deduplicated.
        self.svm.expire_blockhash_for(target);
        result.map(|_| ()).map_err(|f| (f.err, f.meta.logs))
    }

    // ── addresses ──

    fn game(&self) -> Address {
        let (game, _) = Address::find_program_address(
            &[b"game", self.p1.pubkey().as_ref(), &self.game_id.to_le_bytes()],
            &PROGRAM_ID,
        );
        game
    }
    fn board(&self) -> Address {
        Address::find_program_address(&[b"board", self.game().as_ref()], &PROGRAM_ID).0
    }
    fn player(&self, wallet: &Address) -> Address {
        Address::find_program_address(&[b"player", self.game().as_ref(), wallet.as_ref()], &PROGRAM_ID).0
    }
    fn ata(&self, owner: &Address) -> Address {
        get_associated_token_address(owner, &self.mint)
    }
    fn vault(&self) -> Address {
        self.ata(&self.game())
    }

    // ── reads ──

    fn usdc(&self, owner: &Address) -> u64 {
        self.svm
            .get_account(&self.ata(owner))
            .map(|a| TokenAccount::unpack(&a.data).unwrap().amount)
            .unwrap_or(0)
    }
    fn lamports(&self, address: &Address) -> u64 {
        self.svm.get_account(address).map(|a| a.lamports).unwrap_or(0)
    }
    fn base_game(&self) -> Game {
        decode(&self.svm.get_account(&self.game()).expect("game on base"), "Game")
    }
    fn er_board(&self) -> Board {
        decode(&self.svm.get_account_for(ER, &self.board()).expect("board on ER"), "Board")
    }
    fn er_player(&self, wallet: &Address) -> Player {
        decode(&self.svm.get_account_for(ER, &self.player(wallet)).expect("player on ER"), "Player")
    }
    fn er_exists(&self, address: &Address) -> bool {
        self.svm
            .get_account_for(ER, address)
            .is_some_and(|a| a.lamports > 0 || !a.data.is_empty())
    }

    fn warp_seconds(&mut self, seconds: i64) {
        let mut clock: Clock = self.svm.base().get_sysvar();
        clock.unix_timestamp += seconds;
        clock.slot += 1;
        self.svm.set_sysvar(&clock);
    }

    // ── instructions ──

    fn create_ix(&self, creator: &Address, wager: u64) -> Instruction {
        self.create_ix_with(creator, wager, VALIDATOR)
    }

    fn create_ix_with(&self, creator: &Address, wager: u64, validator: Address) -> Instruction {
        let (game, _) = Address::find_program_address(
            &[b"game", creator.as_ref(), &self.game_id.to_le_bytes()],
            &PROGRAM_ID,
        );
        let mut args = self.game_id.to_le_bytes().to_vec();
        args.extend_from_slice(&wager.to_le_bytes());
        args.extend_from_slice(validator.as_ref());
        Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(*creator, true),
                AccountMeta::new(game, false),
                AccountMeta::new_readonly(self.mint, false),
                AccountMeta::new(self.ata(&game), false),
                AccountMeta::new(self.ata(creator), false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
                AccountMeta::new_readonly(ATA_PROGRAM_ID, false),
                AccountMeta::new_readonly(system_program::ID, false),
            ],
            data: ix_data("create_game", &args),
        }
    }

    fn cancel_ix(&self, signer: &Address) -> Instruction {
        Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(*signer, true),
                AccountMeta::new(self.game(), false),
                AccountMeta::new_readonly(self.mint, false),
                AccountMeta::new(self.vault(), false),
                AccountMeta::new(self.ata(signer), false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            ],
            data: ix_data("cancel_game", &[]),
        }
    }

    fn join_ix(&self, joiner: &Address) -> Instruction {
        self.join_ix_with(joiner, WAGER)
    }

    fn join_ix_with(&self, joiner: &Address, expected_wager: u64) -> Instruction {
        Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(*joiner, true),
                AccountMeta::new(self.game(), false),
                AccountMeta::new_readonly(self.mint, false),
                AccountMeta::new(self.vault(), false),
                AccountMeta::new(self.ata(joiner), false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            ],
            data: ix_data("join_game", &expected_wager.to_le_bytes()),
        }
    }

    fn delegate_ix(&self, payer: &Address) -> Instruction {
        let accounts = DelegateAccounts::new(self.game(), PROGRAM_ID);
        Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(*payer, true),
                AccountMeta::new(accounts.delegate_buffer, false),
                AccountMeta::new(accounts.delegation_record, false),
                AccountMeta::new(accounts.delegation_metadata, false),
                AccountMeta::new(self.game(), false),
                AccountMeta::new_readonly(PROGRAM_ID, false),
                AccountMeta::new_readonly(DELEGATION_PROGRAM_ID, false),
                AccountMeta::new_readonly(system_program::ID, false),
            ],
            data: ix_data("delegate_game", &[]),
        }
    }

    fn start_ix(&self, signer: &Address) -> Instruction {
        Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![
                AccountMeta::new_readonly(*signer, true),
                AccountMeta::new(self.game(), false),
                AccountMeta::new(self.board(), false),
                AccountMeta::new(self.player(&self.p1.pubkey()), false),
                AccountMeta::new(self.player(&self.p2.pubkey()), false),
                AccountMeta::new(EPHEMERAL_VAULT_ID, false),
                AccountMeta::new_readonly(MAGIC_PROGRAM_ID, false),
            ],
            data: ix_data("start_game", &[]),
        }
    }

    fn player_ix(&self, name: &str, signer: &Address, args: &[u8]) -> Instruction {
        Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![
                AccountMeta::new_readonly(*signer, true),
                AccountMeta::new_readonly(self.game(), false),
                AccountMeta::new(self.board(), false),
                if name == "play_move" {
                    AccountMeta::new(self.player(signer), false)
                } else {
                    AccountMeta::new_readonly(self.player(signer), false)
                },
            ],
            data: ix_data(name, args),
        }
    }

    fn finish_ix(&self, payer: &Address) -> Instruction {
        Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(*payer, true),
                AccountMeta::new(self.game(), false),
                AccountMeta::new(self.board(), false),
                AccountMeta::new(self.player(&self.p1.pubkey()), false),
                AccountMeta::new(self.player(&self.p2.pubkey()), false),
                AccountMeta::new(EPHEMERAL_VAULT_ID, false),
                AccountMeta::new_readonly(MAGIC_PROGRAM_ID, false),
                AccountMeta::new(MAGIC_CONTEXT_ID, false),
            ],
            data: ix_data("finish_game", &[]),
        }
    }

    fn settle_ix(&self, payer: &Address, player_two: &Address, player_two_token: &Address) -> Instruction {
        let p1 = self.p1.pubkey();
        Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(*payer, true),
                AccountMeta::new(self.game(), false),
                AccountMeta::new(p1, false),
                AccountMeta::new_readonly(*player_two, false),
                AccountMeta::new_readonly(self.mint, false),
                AccountMeta::new(self.vault(), false),
                AccountMeta::new(self.ata(&p1), false),
                AccountMeta::new(*player_two_token, false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
                AccountMeta::new_readonly(ATA_PROGRAM_ID, false),
                AccountMeta::new_readonly(system_program::ID, false),
            ],
            data: ix_data("settle", &[]),
        }
    }

    // ── flows ──

    fn create(&mut self) {
        let ix = self.create_ix(&self.p1.pubkey(), WAGER);
        let p1 = self.p1.insecure_clone();
        self.send(BASE, &[&p1], &[ix]).expect("create_game");
    }

    fn join_and_delegate(&mut self) {
        let p2 = self.p2.insecure_clone();
        let ixs = [self.join_ix(&p2.pubkey()), self.delegate_ix(&p2.pubkey())];
        self.send(BASE, &[&p2], &ixs).expect("join_game + delegate_game");
    }

    fn start(&mut self) {
        let p1 = self.p1.insecure_clone();
        let ix = self.start_ix(&p1.pubkey());
        self.send(ER, &[&p1], &[ix]).expect("start_game");
    }

    fn abort_ix(&self, payer: &Address) -> Instruction {
        Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(*payer, true),
                AccountMeta::new(self.game(), false),
                AccountMeta::new_readonly(self.board(), false),
                AccountMeta::new_readonly(MAGIC_PROGRAM_ID, false),
                AccountMeta::new(MAGIC_CONTEXT_ID, false),
            ],
            data: ix_data("abort_game", &[]),
        }
    }

    fn abort(&mut self, who: &Keypair) -> TxResult {
        let ix = self.abort_ix(&who.pubkey());
        self.send(ER, &[who], &[ix])
    }

    /// Created, joined, delegated and started: ready for moves on the ER.
    fn started() -> Self {
        let mut env = Self::new();
        env.create();
        env.join_and_delegate();
        env.start();
        env
    }

    fn play(&mut self, who: &Keypair, column: u8) -> TxResult {
        let ix = self.player_ix("play_move", &who.pubkey(), &[column]);
        self.send(ER, &[who], &[ix])
    }

    /// Plays alternating moves starting with player one.
    fn play_sequence(&mut self, columns: &[u8]) {
        let (p1, p2) = (self.p1.insecure_clone(), self.p2.insecure_clone());
        for (i, &column) in columns.iter().enumerate() {
            let who = if i % 2 == 0 { &p1 } else { &p2 };
            self.play(who, column)
                .unwrap_or_else(|e| panic!("move {i} col {column}: {e:?}"));
        }
    }

    fn finish(&mut self) -> TxResult {
        let p1 = self.p1.insecure_clone();
        let ix = self.finish_ix(&p1.pubkey());
        self.send(ER, &[&p1], &[ix])
    }

    fn settle(&mut self) -> TxResult {
        // A third party triggers settlement: it is permissionless.
        let stranger = self.stranger.insecure_clone();
        let p2 = self.p2.pubkey();
        let ix = self.settle_ix(&stranger.pubkey(), &p2, &self.ata(&p2));
        self.send(BASE, &[&stranger], &[ix])
    }

    /// Finishes on the ER, checks the ER cleanup + base commit, settles, and checks payouts.
    fn finish_and_settle(&mut self, result: GameResult, reason: EndReason, p1_delta: i64, p2_delta: i64) {
        let (p1, p2) = (self.p1.pubkey(), self.p2.pubkey());
        let p1_lamports_before_settle;
        self.finish().expect("finish_game");

        // Ephemeral accounts are gone from the ER.
        for account in [self.board(), self.player(&p1), self.player(&p2)] {
            assert!(!self.er_exists(&account), "ephemeral account {account} not closed");
        }
        // Ephemeral accounts never existed on base.
        for account in [self.board(), self.player(&p1), self.player(&p2)] {
            assert!(self.svm.get_account(&account).is_none_or(|a| a.lamports == 0));
        }
        // Game is undelegated: owned by the program again on base with the committed result.
        let game_account = self.svm.get_account(&self.game()).unwrap();
        assert_eq!(game_account.owner, PROGRAM_ID, "game not undelegated");
        let game = self.base_game();
        assert_eq!(game.status, GameStatus::Finished);
        assert_eq!(game.result, result);
        assert_eq!(game.end_reason, reason);
        // The sponsor got all ephemeral rent back.
        let rent = Rent::default().minimum_balance(game_account.data.len());
        assert_eq!(game_account.lamports, rent + SPONSOR_LAMPORTS, "sponsor lamports not refunded");

        p1_lamports_before_settle = self.lamports(&p1);
        let vault_lamports = self.lamports(&self.vault());
        self.settle().expect("settle");

        assert_eq!(self.usdc(&p1) as i64, START_USDC as i64 + p1_delta);
        assert_eq!(self.usdc(&p2) as i64, START_USDC as i64 + p2_delta);
        assert!(self.svm.get_account(&self.vault()).is_none_or(|a| a.lamports == 0), "vault not closed");
        assert!(self.svm.get_account(&self.game()).is_none_or(|a| a.lamports == 0), "game not closed");
        // Creator gets back game rent + sponsor lamports + vault rent.
        assert_eq!(
            self.lamports(&p1),
            p1_lamports_before_settle + game_account.lamports + vault_lamports
        );

        // Settling twice is impossible.
        assert!(self.settle().is_err(), "double settle succeeded");
    }
}

fn set_program_account(svm: &mut MagicSVM, address: Address, owner: Address, data: Vec<u8>) {
    let lamports = Rent::default().minimum_balance(data.len());
    svm.set_account(address, Account { lamports, data, owner, executable: false, rent_epoch: 0 })
        .unwrap();
}

#[track_caller]
fn assert_code(result: TxResult, code: u32) {
    match result {
        Err((TransactionError::InstructionError(_, InstructionError::Custom(c)), logs)) => {
            assert_eq!(c, code, "unexpected error code; logs:\n{}", logs.join("\n"))
        }
        Err((other, logs)) => panic!("expected custom error {code}, got {other:?}\n{}", logs.join("\n")),
        Ok(()) => panic!("expected custom error {code}, transaction succeeded"),
    }
}

// ═════════════════════════════ Happy paths ═════════════════════════════

#[test]
fn create_and_join_escrow_wagers_and_delegate_game() {
    let mut env = Env::new();
    let p1_lamports = env.lamports(&env.p1.pubkey());
    env.create();

    let game = env.base_game();
    assert_eq!(game.status, GameStatus::Open);
    assert_eq!(game.wager, WAGER);
    assert_eq!(game.player_one, env.p1.pubkey());
    assert_eq!(game.validator, VALIDATOR);
    assert_eq!(env.usdc(&env.p1.pubkey()), START_USDC - WAGER);
    assert_eq!(env.usdc(&env.game()), WAGER);
    assert!(env.lamports(&env.p1.pubkey()) < p1_lamports - SPONSOR_LAMPORTS);

    env.join_and_delegate();
    assert_eq!(env.usdc(&env.p2.pubkey()), START_USDC - WAGER);
    assert_eq!(env.usdc(&env.game()), 2 * WAGER);
    // Delegated: base owner is the delegation program, ER copy is owned by the program.
    assert_eq!(env.svm.get_account(&env.game()).unwrap().owner, DELEGATION_PROGRAM_ID);
    let er_game = env.svm.get_account_for(ER, &env.game()).unwrap();
    assert_eq!(er_game.owner, PROGRAM_ID);
    let game: Game = decode(&er_game, "Game");
    assert_eq!(game.status, GameStatus::Active);
    assert_eq!(game.player_two, env.p2.pubkey());
    assert!(game.joined_at >= game.created_at);
}

#[test]
fn start_game_creates_ephemeral_board_and_players_on_er_only() {
    let env = Env::started();
    let board = env.er_board();
    assert_eq!(board.game, env.game());
    assert_eq!(board.cells, [0; 42]);
    assert_eq!(board.turn, 1);
    assert_eq!(board.result, GameResult::InProgress);

    let p1 = env.er_player(&env.p1.pubkey());
    let p2 = env.er_player(&env.p2.pubkey());
    assert_eq!((p1.piece, p1.wallet), (1, env.p1.pubkey()));
    assert_eq!((p2.piece, p2.wallet), (2, env.p2.pubkey()));

    // ER-only: nothing exists on Solana for these accounts.
    for account in [env.board(), env.player(&env.p1.pubkey()), env.player(&env.p2.pubkey())] {
        assert!(env.svm.get_account(&account).is_none_or(|a| a.lamports == 0));
        assert_eq!(env.svm.get_account_for(ER, &account).unwrap().owner, PROGRAM_ID);
    }
}

#[test]
fn player_one_wins_vertical_and_takes_the_pot() {
    let mut env = Env::started();
    // P1 stacks column 0, P2 stacks column 1.
    env.play_sequence(&[0, 1, 0, 1, 0, 1, 0]);
    let board = env.er_board();
    assert_eq!(board.result, GameResult::PlayerOneWins);
    assert_eq!(board.move_count, 7);
    assert_eq!(env.er_player(&env.p1.pubkey()).moves, 4);
    assert_eq!(env.er_player(&env.p2.pubkey()).moves, 3);
    env.finish_and_settle(GameResult::PlayerOneWins, EndReason::ConnectFour, WAGER as i64, -(WAGER as i64));
}

#[test]
fn player_two_wins_horizontal_and_takes_the_pot() {
    let mut env = Env::started();
    // P2 builds the bottom row 1..=4 while P1 stacks column 6 then column 5.
    env.play_sequence(&[6, 1, 6, 2, 6, 3, 5, 4]);
    assert_eq!(env.er_board().result, GameResult::PlayerTwoWins);
    env.finish_and_settle(GameResult::PlayerTwoWins, EndReason::ConnectFour, -(WAGER as i64), WAGER as i64);
}

#[test]
fn player_one_wins_diagonal() {
    let mut env = Env::started();
    // P1 diagonal (0,0) (1,1) (2,2) (3,3); P2 fills under it.
    env.play_sequence(&[0, 1, 1, 2, 2, 3, 2, 3, 3, 6, 3]);
    assert_eq!(env.er_board().result, GameResult::PlayerOneWins);
    env.finish_and_settle(GameResult::PlayerOneWins, EndReason::ConnectFour, WAGER as i64, -(WAGER as i64));
}

#[test]
fn full_board_is_a_draw_and_refunds_both() {
    let mut env = Env::started();
    env.play_sequence(&DRAW_SEQUENCE);
    let board = env.er_board();
    assert_eq!(board.result, GameResult::Draw);
    assert_eq!(board.move_count, 42);
    assert!(board.cells.iter().all(|&c| c != 0));
    env.finish_and_settle(GameResult::Draw, EndReason::BoardFull, 0, 0);
}

#[test]
fn waiting_player_claims_win_after_timeout() {
    let mut env = Env::started();
    env.play_sequence(&[3]); // P1 moves, P2 goes silent.
    env.warp_seconds(TURN_TIMEOUT_SECS);
    let p1 = env.p1.insecure_clone();
    let ix = env.player_ix("claim_timeout", &p1.pubkey(), &[]);
    env.send(ER, &[&p1], &[ix]).expect("claim_timeout");
    env.finish_and_settle(GameResult::PlayerOneWins, EndReason::Timeout, WAGER as i64, -(WAGER as i64));
}

#[test]
fn resigning_player_loses() {
    let mut env = Env::started();
    env.play_sequence(&[3, 3]);
    let p1 = env.p1.insecure_clone();
    let ix = env.player_ix("resign", &p1.pubkey(), &[]);
    env.send(ER, &[&p1], &[ix]).expect("resign");
    env.finish_and_settle(GameResult::PlayerTwoWins, EndReason::Resignation, -(WAGER as i64), WAGER as i64);
}

#[test]
fn creator_cancels_open_game_and_is_refunded() {
    let mut env = Env::new();
    let p1 = env.p1.insecure_clone();
    let lamports_before = env.lamports(&p1.pubkey());
    env.create();
    let ix = env.cancel_ix(&p1.pubkey());
    env.send(BASE, &[&p1], &[ix]).expect("cancel_game");
    assert_eq!(env.usdc(&p1.pubkey()), START_USDC);
    assert!(env.svm.get_account(&env.game()).is_none_or(|a| a.lamports == 0));
    assert!(env.svm.get_account(&env.vault()).is_none_or(|a| a.lamports == 0));
    // Everything except the two base transaction fees comes back.
    assert_eq!(env.lamports(&p1.pubkey()), lamports_before - 2 * 5_000);
}

#[test]
fn settle_creates_missing_winner_token_account() {
    let mut env = Env::started();
    // P2 closed its token account mid-game (simulated by removing it).
    let p2_ata = env.ata(&env.p2.pubkey());
    env.svm.set_account(p2_ata, Account::default()).unwrap();
    env.play_sequence(&[6, 1, 6, 2, 6, 3, 5, 4]);
    env.finish().expect("finish_game");
    env.settle().expect("settle");
    assert_eq!(env.usdc(&env.p2.pubkey()), 2 * WAGER);
}

// ═════════════════════════════ Unhappy paths: base layer ═════════════════════════════

#[test]
fn create_rejects_zero_wager() {
    let mut env = Env::new();
    let p1 = env.p1.insecure_clone();
    let ix = env.create_ix(&p1.pubkey(), 0);
    assert_code(env.send(BASE, &[&p1], &[ix]), INVALID_WAGER);
}

#[test]
fn create_fails_without_enough_usdc() {
    let mut env = Env::new();
    let p1 = env.p1.insecure_clone();
    let ix = env.create_ix(&p1.pubkey(), START_USDC + 1);
    // spl-token InsufficientFunds = 1
    assert_code(env.send(BASE, &[&p1], &[ix]), 1);
    assert_eq!(env.usdc(&p1.pubkey()), START_USDC);
}

#[test]
fn creator_cannot_join_own_game() {
    let mut env = Env::new();
    env.create();
    let p1 = env.p1.insecure_clone();
    let ix = env.join_ix(&p1.pubkey());
    assert_code(env.send(BASE, &[&p1], &[ix]), CANNOT_JOIN_OWN_GAME);
}

#[test]
fn third_player_cannot_join_a_full_game() {
    let mut env = Env::new();
    env.create();
    let p2 = env.p2.insecure_clone();
    let ix = env.join_ix(&p2.pubkey());
    env.send(BASE, &[&p2], &[ix]).expect("join without delegate");
    let stranger = env.stranger.insecure_clone();
    let ix = env.join_ix(&stranger.pubkey());
    assert_code(env.send(BASE, &[&stranger], &[ix]), GAME_NOT_OPEN);

    // Once delegated, the base account is owned by the delegation program and is locked.
    let ix = env.delegate_ix(&p2.pubkey());
    env.send(BASE, &[&p2], &[ix]).expect("delegate");
    let ix = env.join_ix(&stranger.pubkey());
    assert_code(env.send(BASE, &[&stranger], &[ix]), ANCHOR_ACCOUNT_OWNED_BY_WRONG_PROGRAM);
}

#[test]
fn open_game_cannot_be_delegated() {
    let mut env = Env::new();
    env.create();
    let p1 = env.p1.insecure_clone();
    let ix = env.delegate_ix(&p1.pubkey());
    assert_code(env.send(BASE, &[&p1], &[ix]), GAME_NOT_ACTIVE);
}

#[test]
fn only_creator_can_cancel_and_only_while_open() {
    let mut env = Env::new();
    env.create();
    let stranger = env.stranger.insecure_clone();
    let ix = env.cancel_ix(&stranger.pubkey());
    assert_code(env.send(BASE, &[&stranger], &[ix]), ANCHOR_CONSTRAINT_HAS_ONE);

    let p2 = env.p2.insecure_clone();
    let ix = env.join_ix(&p2.pubkey());
    env.send(BASE, &[&p2], &[ix]).expect("join");
    let p1 = env.p1.insecure_clone();
    let ix = env.cancel_ix(&p1.pubkey());
    assert_code(env.send(BASE, &[&p1], &[ix]), GAME_NOT_OPEN);
    assert_eq!(env.usdc(&env.game()), 2 * WAGER);
}

#[test]
fn settle_is_rejected_while_game_is_delegated() {
    let mut env = Env::started();
    env.play_sequence(&[0, 1, 0, 1, 0, 1, 0]);
    // Board has a winner but the game was never committed back: base still locked.
    assert_code(env.settle(), ANCHOR_ACCOUNT_OWNED_BY_WRONG_PROGRAM);
    assert_eq!(env.usdc(&env.game()), 2 * WAGER);
}

#[test]
fn settle_rejects_wrong_player_or_token_account() {
    let mut env = Env::started();
    env.play_sequence(&[6, 1, 6, 2, 6, 3, 5, 4]); // P2 wins
    env.finish().expect("finish_game");
    let stranger = env.stranger.insecure_clone();

    // Attacker passes themselves as player two.
    let ix = env.settle_ix(&stranger.pubkey(), &stranger.pubkey(), &env.ata(&stranger.pubkey()));
    assert_code(env.send(BASE, &[&stranger], &[ix]), ANCHOR_CONSTRAINT_HAS_ONE);

    // Attacker passes the real player two but their own token account.
    let ix = env.settle_ix(&stranger.pubkey(), &env.p2.pubkey(), &env.ata(&stranger.pubkey()));
    assert!(env.send(BASE, &[&stranger], &[ix]).is_err());

    assert_eq!(env.usdc(&stranger.pubkey()), START_USDC);
    env.settle().expect("honest settle");
    assert_eq!(env.usdc(&env.p2.pubkey()), START_USDC + WAGER);
}

#[test]
fn gameplay_instructions_fail_on_base_layer() {
    let mut env = Env::new();
    env.create();
    env.join_and_delegate();
    // start_game on base: the game is owned by the delegation program there.
    let p2 = env.p2.insecure_clone();
    let ix = env.start_ix(&p2.pubkey());
    assert!(env.send(BASE, &[&p2], &[ix]).is_err());
    env.start();
    // Moves on base: board does not exist there.
    let p1 = env.p1.insecure_clone();
    let ix = env.player_ix("play_move", &p1.pubkey(), &[0]);
    assert!(env.send(BASE, &[&p1], &[ix]).is_err());
}

// ═════════════════════════════ Unhappy paths: ephemeral rollup ═════════════════════════════

#[test]
fn start_game_rejects_non_players_and_double_start() {
    let mut env = Env::new();
    env.create();
    env.join_and_delegate();
    for who in [env.stranger.insecure_clone(), env.p2.insecure_clone()] {
        let ix = env.start_ix(&who.pubkey());
        assert_code(env.send(ER, &[&who], &[ix]), ONLY_CREATOR_CAN_START);
    }
    assert!(!env.er_exists(&env.board()));

    env.start();
    let p1 = env.p1.insecure_clone();
    let ix = env.start_ix(&p1.pubkey());
    assert!(env.send(ER, &[&p1], &[ix]).is_err(), "second start_game succeeded");
    assert_eq!(env.er_board().turn, 1);
}

#[test]
fn moves_out_of_turn_are_rejected() {
    let mut env = Env::started();
    let (p1, p2) = (env.p1.insecure_clone(), env.p2.insecure_clone());
    assert_code(env.play(&p2, 0), NOT_YOUR_TURN);
    env.play(&p1, 0).unwrap();
    assert_code(env.play(&p1, 0), NOT_YOUR_TURN);
    assert_eq!(env.er_board().move_count, 1);
}

#[test]
fn outsider_cannot_move_resign_or_claim() {
    let mut env = Env::started();
    let stranger = env.stranger.insecure_clone();
    // play_move writes the (non-existent) player record: the ER refuses the transaction
    // before the program runs because the writable account is not delegated / ephemeral.
    match env.play(&stranger, 0) {
        Err((TransactionError::InvalidWritableAccount, _)) => {}
        other => panic!("expected InvalidWritableAccount, got {other:?}"),
    }
    // resign / claim_timeout only read the player record: the program rejects it.
    for name in ["resign", "claim_timeout"] {
        let mut ix = env.player_ix(name, &stranger.pubkey(), &[]);
        ix.accounts[3].is_writable = false;
        assert_code(env.send(ER, &[&stranger], &[ix]), ANCHOR_ACCOUNT_NOT_INITIALIZED);
    }
    assert_eq!(env.er_board().move_count, 0);
}

#[test]
fn player_cannot_use_opponents_player_account() {
    let mut env = Env::started();
    let p2 = env.p2.insecure_clone();
    // P2 signs but passes P1's player record (whose turn it is).
    let mut ix = env.player_ix("play_move", &p2.pubkey(), &[0]);
    ix.accounts[3] = AccountMeta::new(env.player(&env.p1.pubkey()), false);
    assert_code(env.send(ER, &[&p2], &[ix]), ANCHOR_CONSTRAINT_SEEDS);
}

#[test]
fn invalid_and_full_columns_are_rejected() {
    let mut env = Env::started();
    let (p1, p2) = (env.p1.insecure_clone(), env.p2.insecure_clone());
    assert_code(env.play(&p1, 7), INVALID_COLUMN);
    assert_code(env.play(&p1, 255), INVALID_COLUMN);
    // Fill column 0 without anyone winning: alternate pieces.
    env.play_sequence(&[0, 0, 0, 0, 0, 0]);
    assert_code(env.play(&p1, 0), INVALID_COLUMN);
    env.play(&p1, 1).unwrap();
    env.play(&p2, 1).unwrap();
    assert_eq!(env.er_board().move_count, 8);
}

#[test]
fn no_moves_after_the_game_is_won() {
    let mut env = Env::started();
    env.play_sequence(&[0, 1, 0, 1, 0, 1, 0]);
    let (p1, p2) = (env.p1.insecure_clone(), env.p2.insecure_clone());
    assert_code(env.play(&p2, 1), GAME_OVER);
    assert_code(env.play(&p1, 2), GAME_OVER);
    for (name, who) in [("resign", &p2), ("claim_timeout", &p2)] {
        let ix = env.player_ix(name, &who.pubkey(), &[]);
        assert_code(env.send(ER, &[who], &[ix]), GAME_OVER);
    }
    assert_eq!(env.er_board().result, GameResult::PlayerOneWins);
}

#[test]
fn timeout_rules() {
    let mut env = Env::started();
    env.play_sequence(&[3]);
    let (p1, p2) = (env.p1.insecure_clone(), env.p2.insecure_clone());

    // Too early.
    env.warp_seconds(TURN_TIMEOUT_SECS - 1);
    let ix = env.player_ix("claim_timeout", &p1.pubkey(), &[]);
    assert_code(env.send(ER, &[&p1], &[ix]), TIMEOUT_NOT_REACHED);

    // The stalling player (on turn) cannot claim the timeout for themselves.
    env.warp_seconds(10);
    let ix = env.player_ix("claim_timeout", &p2.pubkey(), &[]);
    assert_code(env.send(ER, &[&p2], &[ix]), CANNOT_CLAIM_OWN_TIMEOUT);

    // A late move still counts if nobody claimed; it resets the clock.
    env.play(&p2, 3).unwrap();
    let ix = env.player_ix("claim_timeout", &p2.pubkey(), &[]);
    assert_code(env.send(ER, &[&p2], &[ix]), TIMEOUT_NOT_REACHED);
}

#[test]
fn finish_rejected_while_in_progress_and_only_once() {
    let mut env = Env::started();
    env.play_sequence(&[0, 1]);
    assert_code(env.finish(), GAME_IN_PROGRESS);
    assert_eq!(env.svm.get_account(&env.game()).unwrap().owner, DELEGATION_PROGRAM_ID);

    env.play_sequence(&[0, 1, 0, 1, 0]);
    env.finish().expect("finish_game");
    assert!(env.finish().is_err(), "second finish_game succeeded");
}

#[test]
fn finished_game_cannot_be_delegated_again() {
    let mut env = Env::started();
    env.play_sequence(&[0, 1, 0, 1, 0, 1, 0]);
    env.finish().expect("finish_game");
    let p2 = env.p2.insecure_clone();
    let ix = env.delegate_ix(&p2.pubkey());
    assert_code(env.send(BASE, &[&p2], &[ix]), GAME_NOT_ACTIVE);
}

// ═════════════════════════════ Audit regressions ═════════════════════════════

#[test]
fn create_rejects_validator_outside_allowlist() {
    let mut env = Env::new();
    let p1 = env.p1.insecure_clone();
    let ix = env.create_ix_with(&p1.pubkey(), WAGER, Address::new_unique());
    assert_code(env.send(BASE, &[&p1], &[ix]), VALIDATOR_NOT_ALLOWED);
}

#[test]
fn join_rejects_changed_wager() {
    // Creator cancels and re-creates the same id with a bigger wager before the join lands.
    let mut env = Env::new();
    env.create();
    let p1 = env.p1.insecure_clone();
    let ix = env.cancel_ix(&p1.pubkey());
    env.send(BASE, &[&p1], &[ix]).unwrap();
    let ix = env.create_ix(&p1.pubkey(), 10 * WAGER);
    env.send(BASE, &[&p1], &[ix]).unwrap();
    let p2 = env.p2.insecure_clone();
    let ix = env.join_ix_with(&p2.pubkey(), WAGER);
    assert_code(env.send(BASE, &[&p2], &[ix]), WAGER_MISMATCH);
    assert_eq!(env.usdc(&p2.pubkey()), START_USDC);
}

#[test]
fn create_succeeds_when_vault_ata_was_precreated() {
    let mut env = Env::new();
    let vault_owner = env.game();
    env.set_token_account(vault_owner, 0);
    env.create();
    assert_eq!(env.usdc(&env.game()), WAGER);
}

#[test]
fn never_started_game_is_aborted_and_refunded() {
    let mut env = Env::new();
    env.create();
    env.join_and_delegate();
    let (p1, p2) = (env.p1.insecure_clone(), env.p2.insecure_clone());

    assert_code(env.abort(&p2), START_TIMEOUT_NOT_REACHED);
    env.warp_seconds(START_TIMEOUT_SECS);
    let stranger = env.stranger.insecure_clone();
    assert_code(env.abort(&stranger), NOT_A_PLAYER);

    env.abort(&p2).expect("abort_game");
    let game = env.base_game();
    assert_eq!(env.svm.get_account(&env.game()).unwrap().owner, PROGRAM_ID);
    assert_eq!((game.status, game.result, game.end_reason), (GameStatus::Finished, GameResult::Draw, EndReason::Aborted));
    // Too late to start now.
    let ix = env.start_ix(&p1.pubkey());
    assert!(env.send(ER, &[&p1], &[ix]).is_err());
    env.settle().expect("settle");
    assert_eq!(env.usdc(&p1.pubkey()), START_USDC);
    assert_eq!(env.usdc(&p2.pubkey()), START_USDC);
}

#[test]
fn started_game_cannot_be_aborted() {
    let mut env = Env::started();
    env.warp_seconds(START_TIMEOUT_SECS + 1);
    let p2 = env.p2.insecure_clone();
    assert_code(env.abort(&p2), GAME_ALREADY_STARTED);
    // The stalled first move is handled by the turn timeout instead.
    let ix = env.player_ix("claim_timeout", &p2.pubkey(), &[]);
    env.send(ER, &[&p2], &[ix]).expect("claim_timeout");
}

#[test]
fn squatted_board_address_cannot_lock_funds() {
    let mut env = Env::new();
    env.create();
    env.join_and_delegate();
    // Attacker pre-funds the board PDA address so the ER sees a non-empty system account there
    // (on a live ER the funded base account is cloned in; here we place it on the ER ledger).
    let board = env.board();
    env.svm.airdrop(&board, 1_000_000).unwrap();
    env.svm
        .ephemeral_mut()
        .set_account(board, magicblock_account::Account::new(1_000_000, 0, &system_program::ID))
        .unwrap();
    let p1 = env.p1.insecure_clone();
    let ix = env.start_ix(&p1.pubkey());
    assert!(env.send(ER, &[&p1], &[ix]).is_err(), "start_game should fail on a squatted board");

    // Escape hatch: refund both after the start timeout.
    env.warp_seconds(START_TIMEOUT_SECS);
    env.abort(&p1).expect("abort_game");
    env.settle().expect("settle");
    assert_eq!(env.usdc(&p1.pubkey()), START_USDC);
    assert_eq!(env.usdc(&env.p2.pubkey()), START_USDC);
}

use anchor_lang::prelude::*;

pub const GAME_SEED: &[u8] = b"game";
pub const BOARD_SEED: &[u8] = b"board";
pub const PLAYER_SEED: &[u8] = b"player";

pub const ROWS: usize = 6;
pub const COLS: usize = 7;
pub const CELLS: usize = ROWS * COLS;

/// Seconds the player on turn has to move before the opponent may claim the win.
pub const TURN_TIMEOUT_SECS: i64 = 60;

/// Seconds after the join after which either player may abort (full refund) a game whose
/// board was never created on the ER (creator never started it, or creation was blocked).
pub const START_TIMEOUT_SECS: i64 = 120;

/// MagicBlock ER validators a game may be delegated to. Commits only work from validators
/// MagicBlock has whitelisted, so delegating anywhere else would lock the escrow.
pub const ALLOWED_VALIDATORS: [Pubkey; 4] = [
    pubkey!("MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57"), // devnet Asia
    pubkey!("MEUGGrYPxKk17hCr7wpT6s8dtNokZj5U2L57vjYMS8e"), // devnet Europe
    pubkey!("MUS3hc9TCw4cGC12vHNoYcCGzJG1txjgQLZWVoeNHNd"), // devnet USA
    pubkey!("mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev"), // local ER (mb-stack)
];

/// Extra lamports the creator puts on the game PDA so it can sponsor the ER rent of the
/// three ephemeral accounts (~15k lamports). All of it comes back on settle/cancel.
pub const SPONSOR_LAMPORTS: u64 = 100_000;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum GameStatus {
    /// Created on Solana, waiting for an opponent.
    Open,
    /// Both wagers escrowed; game is (or is about to be) delegated and played on the ER.
    Active,
    /// Result committed back to Solana; waiting for `settle`.
    Finished,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum GameResult {
    InProgress,
    PlayerOneWins,
    PlayerTwoWins,
    Draw,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum EndReason {
    None,
    ConnectFour,
    BoardFull,
    Timeout,
    Resignation,
    /// Never started on the ER; both wagers refunded.
    Aborted,
}

/// Durable escrow + result record. Lives on Solana, is delegated to the ER while the game
/// is played, sponsors the ephemeral accounts, and is committed back with the result.
#[account]
#[derive(InitSpace)]
pub struct Game {
    pub id: u64,
    pub player_one: Pubkey,
    pub player_two: Pubkey,
    pub mint: Pubkey,
    pub wager: u64,
    /// ER validator both players agreed to play on (chosen by the creator).
    pub validator: Pubkey,
    pub status: GameStatus,
    pub result: GameResult,
    pub end_reason: EndReason,
    pub created_at: i64,
    pub joined_at: i64,
    pub bump: u8,
}

/// ER-only board. Created when the game starts on the ER and closed when it ends.
#[account]
#[derive(InitSpace)]
pub struct Board {
    pub game: Pubkey,
    /// Row-major, row 0 is the bottom. 0 = empty, 1 = player one, 2 = player two.
    pub cells: [u8; CELLS],
    /// 1 or 2: whose turn it is.
    pub turn: u8,
    pub move_count: u8,
    /// Column of the last move, 255 before the first move.
    pub last_column: u8,
    pub last_move_ts: i64,
    pub result: GameResult,
    pub end_reason: EndReason,
    pub bump: u8,
}

/// ER-only per-player record. Created when the game starts on the ER and closed when it ends.
#[account]
#[derive(InitSpace)]
pub struct Player {
    pub game: Pubkey,
    pub wallet: Pubkey,
    /// 1 for player one (moves first), 2 for player two.
    pub piece: u8,
    pub moves: u8,
    pub bump: u8,
}

impl Board {
    /// Drops `piece` into `column` and returns `(row, connected_four)`.
    /// Fails if the column is out of range or full.
    pub fn drop_piece(&mut self, column: usize, piece: u8) -> Option<(usize, bool)> {
        if column >= COLS {
            return None;
        }
        let row = (0..ROWS).find(|&r| self.cells[r * COLS + column] == 0)?;
        self.cells[row * COLS + column] = piece;

        let at = |r: i64, c: i64| -> bool {
            (0..ROWS as i64).contains(&r)
                && (0..COLS as i64).contains(&c)
                && self.cells[r as usize * COLS + c as usize] == piece
        };
        // Count matching pieces through the new piece along each axis.
        let (r0, c0) = (row as i64, column as i64);
        let connected = [(0, 1), (1, 0), (1, 1), (1, -1)].iter().any(|&(dr, dc)| {
            let mut count = 1;
            for sign in [1, -1] {
                let mut step = 1;
                while at(r0 + sign * dr * step, c0 + sign * dc * step) {
                    count += 1;
                    step += 1;
                }
            }
            count >= 4
        });
        Some((row, connected))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_board() -> Board {
        Board {
            game: Pubkey::default(),
            cells: [0; CELLS],
            turn: 1,
            move_count: 0,
            last_column: 255,
            last_move_ts: 0,
            result: GameResult::InProgress,
            end_reason: EndReason::None,
            bump: 0,
        }
    }

    #[test]
    fn detects_all_four_directions() {
        // horizontal
        let mut b = empty_board();
        for c in 0..3 {
            assert!(!b.drop_piece(c, 1).unwrap().1);
        }
        assert!(b.drop_piece(3, 1).unwrap().1);

        // vertical
        let mut b = empty_board();
        for _ in 0..3 {
            assert!(!b.drop_piece(5, 2).unwrap().1);
        }
        assert!(b.drop_piece(5, 2).unwrap().1);

        // diagonal up-right, completed in the middle
        let mut b = empty_board();
        let filler = [(1, 1), (2, 2), (3, 3)];
        for (col, height) in filler {
            for _ in 0..height {
                b.drop_piece(col, 2);
            }
        }
        b.drop_piece(0, 1);
        b.drop_piece(1, 1);
        b.drop_piece(3, 1);
        assert!(b.drop_piece(2, 1).unwrap().1);

        // diagonal up-left
        let mut b = empty_board();
        for (col, height) in [(5, 1), (4, 2), (3, 3)] {
            for _ in 0..height {
                b.drop_piece(col, 2);
            }
        }
        b.drop_piece(6, 1);
        b.drop_piece(5, 1);
        b.drop_piece(4, 1);
        assert!(b.drop_piece(3, 1).unwrap().1);
    }

    #[test]
    fn rejects_full_and_out_of_range_columns() {
        let mut b = empty_board();
        for i in 0..ROWS {
            assert!(b.drop_piece(0, 1 + (i % 2) as u8).is_some());
        }
        assert!(b.drop_piece(0, 1).is_none());
        assert!(b.drop_piece(COLS, 1).is_none());
    }
}

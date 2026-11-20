use anchor_lang::prelude::*;

#[error_code]
pub enum ConnectFourError {
    #[msg("Wager must be greater than zero")]
    InvalidWager,
    #[msg("Game is not open for joining")]
    GameNotOpen,
    #[msg("Game is not active")]
    GameNotActive,
    #[msg("Game is not finished")]
    GameNotFinished,
    #[msg("You cannot join your own game")]
    CannotJoinOwnGame,
    #[msg("Signer is not a player in this game")]
    NotAPlayer,
    #[msg("Game is already over")]
    GameOver,
    #[msg("Game is still in progress")]
    GameInProgress,
    #[msg("It is not your turn")]
    NotYourTurn,
    #[msg("Column is out of range or full")]
    InvalidColumn,
    #[msg("Opponent's turn has not timed out yet")]
    TimeoutNotReached,
    #[msg("Only the player waiting for the opponent can claim a timeout")]
    CannotClaimOwnTimeout,
    #[msg("Validator is not an allowed MagicBlock ER validator")]
    ValidatorNotAllowed,
    #[msg("Wager does not match what the joiner expected")]
    WagerMismatch,
    #[msg("Only the game creator can start the game")]
    OnlyCreatorCanStart,
    #[msg("Game was already started; it cannot be aborted")]
    GameAlreadyStarted,
    #[msg("Start timeout has not been reached yet")]
    StartTimeoutNotReached,
}

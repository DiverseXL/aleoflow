// The {{PROJECT_NAME}} program
// A basic GameFi program with a player score record.

program {{PROJECT_NAME}}.aleo {
    @noupgrade
    constructor() {}

    // A record representing a player's score and game state.
    record PlayerState {
        owner: address,
        score: u64,
        level: u64,
        last_played: u64,
    }

    // Submit a new score and update the player's state.
    fn submit_score(player: address, new_score: u64, new_level: u64, timestamp: u64) -> PlayerState {
        return PlayerState {
            owner: player,
            score: new_score,
            level: new_level,
            last_played: timestamp,
        };
    }
}

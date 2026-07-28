// The {{PROJECT_NAME}} program
// A basic DeFi program with deposit and withdraw functions.

program {{PROJECT_NAME}}.aleo {
    @noupgrade
    constructor() {}

    // Deposit tokens into the pool.
    fn deposit(amount: u64) -> u64 {
        assert_neq(amount, 0u64);
        return amount;
    }

    // Withdraw tokens from the pool.
    fn withdraw(amount: u64) -> u64 {
        assert_neq(amount, 0u64);
        return amount;
    }
}

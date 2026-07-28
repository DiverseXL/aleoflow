// The {{PROJECT_NAME}} program
// A basic payment transfer program for the Aleo blockchain.

program {{PROJECT_NAME}}.aleo {
    @noupgrade
    constructor() {}

    // Transfer tokens from sender to receiver.
    // In a real application, this would check balances and update records.
    fn transfer(amount: u64) -> u64 {
        assert_neq(amount, 0u64);
        return amount;
    }
}

// Test file for the {{PROJECT_NAME}} DeFi program.
// Tests: deposit and withdraw return the amount unchanged for valid inputs.

import {{PROJECT_NAME}}.aleo;

program test_program.aleo {
    @noupgrade
    constructor() {}

    @test
    fn test_deposit() {
        let result = {{PROJECT_NAME}}.aleo::deposit(100u64);
        assert_eq(result, 100u64);
    }

    @test
    fn test_withdraw() {
        let result = {{PROJECT_NAME}}.aleo::withdraw(50u64);
        assert_eq(result, 50u64);
    }
}

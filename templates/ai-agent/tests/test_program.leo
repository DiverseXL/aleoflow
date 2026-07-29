// Test file for the {{PROJECT_NAME}} AI Agent program.
// Tests: run_inference returns a record with the expected confidence field.

import {{PROJECT_NAME}}.aleo;

program test_program.aleo {
    @noupgrade
    constructor() {}

    @test
    fn test_run_inference() {
        let result = {{PROJECT_NAME}}.aleo::run_inference(
            1field,
            aleo172leh6aw467q7jzer533wcj8dzzsat3kr446g3nys3pmgqljcs8sfea3eh,
        );
        assert_eq(result.confidence, 0u64);
    }
}

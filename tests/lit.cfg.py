# llvm-lit configuration for coblang ported COBOL test suites.
import os
import lit.formats

config.name = "coblang"
config.test_format = lit.formats.ShTest(execute_external=True)
config.suffixes = [".lit.test"]
config.test_source_root = os.path.dirname(__file__)
config.test_exec_root = os.environ.get(
    "COBLANG_LIT_EXEC_ROOT", os.path.join(config.test_source_root, ".tmp", "lit")
)

coblang_test_bin = os.environ.get("COBLANG_TEST_BIN", "coblang-test")
config.substitutions.append(("%coblang_test", coblang_test_bin))

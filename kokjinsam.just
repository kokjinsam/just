set positional-arguments

help:
    @.just/bin/workflow help

[positional-arguments]
install *args:
    @.just/bin/workflow install "$@"

[positional-arguments]
remove *args:
    @.just/bin/workflow remove "$@"

[positional-arguments]
check *args:
    @JUST_CALLER_DIR={{ quote(invocation_directory()) }} .just/bin/workflow check "$@"

[positional-arguments]
dev *args:
    @.just/bin/workflow dev "$@"

[positional-arguments]
lint *args:
    @JUST_CALLER_DIR={{ quote(invocation_directory()) }} .just/bin/workflow lint "$@"

[positional-arguments]
migrate *args:
    @.just/bin/workflow migrate "$@"

[positional-arguments]
format *args:
    @JUST_CALLER_DIR={{ quote(invocation_directory()) }} .just/bin/workflow format "$@"

[positional-arguments]
reset *args:
    @.just/bin/workflow reset "$@"

[positional-arguments]
test *args:
    @JUST_CALLER_DIR={{ quote(invocation_directory()) }} .just/bin/workflow test "$@"

[positional-arguments]
build *args:
    @JUST_CALLER_DIR={{ quote(invocation_directory()) }} .just/bin/workflow build "$@"

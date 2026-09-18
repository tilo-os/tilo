# Version 1 encrypted backup fixture

`backup-v1.tilopw` was created by the native Windows app before the checked-header
parser change, during an isolated integration test. It contains one synthetic
login, not user data. The test passphrase is `Owned backup fixture 52!`; the login
password is `Owned fixture password 92!`. Both are public test values.

The fixture verifies compatibility with existing backups independently of the
current exporter. Do not replace it when changing the exporter or parser.

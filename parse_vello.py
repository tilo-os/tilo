import sys
import json

with open("target/doc/search-index.js", "r") as f:
    content = f.read()
    # It sets a global window.searchIndex
    print(content[:500])

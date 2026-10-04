api-variables-resolved = { $count ->
    [one] 1 variable resolved
   *[other] { $count } variables resolved
}
api-variables-unresolved = { $count ->
    [one] Unresolved variable: { $names }
   *[other] Unresolved variables: { $names }
}

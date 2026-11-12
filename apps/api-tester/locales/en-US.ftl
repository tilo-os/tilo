api-variables-resolved = { $count ->
    [one] 1 variable resolved
   *[other] { $count } variables resolved
}
api-variables-unresolved = { $count ->
    [one] Unresolved variable: { $names }
   *[other] Unresolved variables: { $names }
}

## History: what each change did, by field and by verb

api-tester-workspace-collections = Changed the collections
api-tester-workspace-environments = Changed the environments
api-tester-workspace-active-environment = Changed which environment is active
api-tester-workspace-request = Replaced the request in the builder
api-tester-request-record-name = Renamed a request
api-tester-request-record-auth = Changed the authentication of a request
api-tester-request-record-method = Set the method to { $new ->
    [get] GET
    [post] POST
    [put] PUT
    [patch] PATCH
    [delete] DELETE
    [head] HEAD
    [options] OPTIONS
   *[other] { $new }
}
api-tester-request-record-url = Edited the URL of a request
api-tester-request-record-params = Changed the query parameters of a request
api-tester-request-record-headers = Changed the headers of a request
api-tester-request-record-body-mode = Set the body to { $new ->
    [none] none
    [json] JSON
    [form] a form
    [raw] raw text
   *[other] { $new }
}
api-tester-request-record-body = Edited the body of a request
api-tester-request-record-form = Changed the form fields of a request
api-tester-credentials-scheme = Set the authentication to { $new ->
    [none] none
    [bearer] a bearer token
    [basic] basic
    [api_key] an API key
   *[other] { $new }
}
api-tester-credentials-username = Changed the user name of a request
api-tester-credentials-header = Changed the key header of a request
api-tester-row-key = Changed the key of a row
api-tester-row-value = Changed the value of a row
api-tester-row-enabled = { $new ->
    [true] Included a row
   *[false] Excluded a row
}
api-tester-saved-collection-name = Renamed the collection { $item }
api-tester-saved-collection-requests = Changed the requests of { $item }
api-tester-environment-name = Renamed the environment { $item }
api-tester-environment-variables = Changed the variables of { $item }
api-tester-variable-key = Renamed a variable of { $item }
api-tester-variable-value = Changed a variable of { $item }
api-tester-variable-enabled = { $new ->
    [true] Included a variable of { $item }
   *[false] Excluded a variable of { $item }
}
api-tester-new-request = Started a new request
api-tester-new-collection = Created the collection { $name }
api-tester-delete-collection = Deleted the collection { $item }
api-tester-add-request = Added { $name } to { $item }
api-tester-save-request = Saved a request to { $item }
api-tester-open-request = Opened a request from { $item }
api-tester-delete-request = Deleted a request from { $item }
api-tester-new-environment = Created the environment { $name }
api-tester-delete-environment = Deleted the environment { $item }
api-tester-use-environment = Switched to the environment { $item }
api-tester-add-variable = Added the variable { $key } to { $item }
api-tester-open-sent = Opened { $item } again
api-tester-record-sent = Sent a request that returned status { $status }

## Accessibility
api-tester-row-actions = Row actions
api-tester-url-placeholder = Enter a URL

## Window and accessibility copy
api-tester-environment = Environment
api-tester-a11y-use-row = Use this row
api-tester-request-body = Request body
api-tester-request-url = Request URL
api-tester-method = Method
api-tester-response-headers = Response headers
api-tester-requests = Requests
api-tester-add-row = Add { $field }

## What the window shows
api-tester-name = Name
api-tester-variable-key-field = Variable key
api-tester-variable-value-field = Variable value
api-tester-key = Key
api-tester-value = Value
api-tester-add-key = Add a key
api-tester-add-value = Add a value
api-tester-remove-row = Remove row
api-tester-save-request-button = Save request
api-tester-cancel-request = Cancel request
api-tester-send = Send
api-tester-query-parameters = Query parameters
api-tester-headers = Headers
api-tester-token = Token
api-tester-username = Username
api-tester-password = Password
api-tester-header-name = Header name
api-tester-copy-body = Copy body
api-tester-copy-headers = Copy headers
api-tester-copy-request = Copy request
api-tester-new-collection-button = New collection
api-tester-reset-area = Reset request area

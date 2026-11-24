# Shared Start-menu names for installation and catalog verification.
function Get-TiloWindowsAppName {
    param([Parameter(Mandatory)][string]$Application)
    switch ($Application) {
        'file-explorer' { 'Files' }
        'word-processor' { 'Word Processor' }
        'hex-editor' { 'Hex Editor' }
        'api-tester' { 'API Tester' }
        'diffmerge' { 'Diff and Merge' }
        'db-client' { 'Database Client' }
        'ebook' { 'Books' }
        default { (Get-Culture).TextInfo.ToTitleCase($Application) }
    }
}

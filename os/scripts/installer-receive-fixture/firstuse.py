"""Actual installed first-use actions; observations never substitute retirement."""
import time


def complete(control, account, password, backup_destination, expected_operation, expected_apps_stage):
    import_started = False
    backup_saved = False
    migration_states = []
    credentials_delivered = 0
    last = None
    rw_available = False
    apps_copy_cleanup = None
    while time.monotonic() < control.deadline:
        rows = control.rows()
        onboarding = {row['id']: row for row in rows if row.get('provider') == 'onboarding'}
        if 'inspect' not in onboarding:
            # Only the actual normal post-firstuse surface may conclude this
            # action sequence; a vanished view alone does not prove completion.
            if (import_started and rw_available and apps_copy_cleanup is not None
                    and any(row.get('provider') == 'applications' for row in rows)):
                return {'migration_states': migration_states,
                        'credentials_delivered': credentials_delivered,
                        'backup_saved': backup_saved,
                        'rw_available': rw_available,
                        'apps_copy_cleanup': apps_copy_cleanup,
                        'certified_owner_surface': True}
            raise RuntimeError('first-use disappeared before normal application surface')
        state = control.inspect()
        if state.get('failed'):
            raise RuntimeError('actual first-use reducer reported failure')
        credentials_delivered += control.answer_authentication(account, password)
        prepared = state.get('prepared')
        if prepared is not None:
            if prepared == 'ready' and not import_started:
                control.invoke(control.offered('onboarding', 'prepared.import'), {'password': password})
                import_started = True
            elif prepared not in ('ready', 'busy'):
                raise RuntimeError('unexpected actual prepared import state')
            time.sleep(0.05)
            continue
        migration = state.get('migration')
        if migration and migration.get('deferred') is not True:
            if (migration.get('selected') not in (None, expected_operation)
                    or expected_operation not in migration.get('operations', [])):
                raise RuntimeError('actual migration belongs to another installed source')
            current = migration.get('state')
            if current != last:
                migration_states.append(current)
                last = current
            if migration.get('cleanup_state') == 'unavailable':
                raise RuntimeError('actual source cleanup custody is unavailable')
            if current == 'existing-files-available':
                result = migration.get('original_copy_completion') or {}
                cleanup = migration.get('cleanup_result') or {}
                digest = result.get('applications_cipher_sha256')
                if (migration.get('retained_access') is not True
                        or migration.get('transfer_complete') is not True
                        or migration.get('cleanup_state') != 'finished'
                        or result.get('event') != 'complete'
                        or result.get('operation') != expected_apps_stage
                        or result.get('home_cipher_sha256') is not None
                        or not isinstance(digest, str) or len(digest) != 64
                        or any(ch not in '0123456789abcdef' for ch in digest)
                        or cleanup.get('event') != 'cleanup-finished'
                        or cleanup.get('operation') != expected_apps_stage
                        or type(cleanup.get('metadata_quarantined')) is not bool):
                    raise RuntimeError('RW availability lacks actual completed Apps/retirement join')
                rw_available = True
                apps_copy_cleanup = cleanup

            # These are actual offered reducer actions. Every route still
            # replays the protected root declaration, selection and person.
            choices = ('migration.cleanup-resume', 'migration.review', 'migration.start',
                       'migration.adopt', 'migration.continue')
            action = next((onboarding[key] for key in choices if key in onboarding), None)
            if action is not None:
                args = {'operation': expected_operation} if action['id'] == 'migration.review' else {}
                for param in action.get('params', []):
                    if param.get('name') == 'password':
                        args['password'] = password
                control.invoke(action, args)
            elif current not in ('reviewing', 'copying', 'verifying', 'adopting',
                                  'restoring-applications', 'releasing-applications',
                                  'retirement-pending', 'cancelling', 'using-existing-files',
                                  'existing-files-available'):
                raise RuntimeError('migration is not progressing through an offered action')
            time.sleep(0.05)
            continue
        if state.get('backup') is not None:
            if 'backup.save' in onboarding and not backup_saved:
                control.invoke(onboarding['backup.save'], {'destination': str(backup_destination),
                               'passphrase': password, 'confirmation': password})
                backup_saved = True
            elif 'backup.continue' in onboarding and backup_saved:
                control.invoke(onboarding['backup.continue'])
            else:
                time.sleep(0.05)
            continue
        offered = next((onboarding[key] for key in
                        ('onboarding.codecs.skip', 'onboarding.codecs.continue',
                         'onboarding.tour.skip', 'onboarding.finish') if key in onboarding), None)
        if offered is None:
            raise RuntimeError('unexpected identity ceremony after authenticated prepared import')
        control.invoke(offered)
        time.sleep(0.05)
    raise RuntimeError('actual receiving first-use deadline elapsed')

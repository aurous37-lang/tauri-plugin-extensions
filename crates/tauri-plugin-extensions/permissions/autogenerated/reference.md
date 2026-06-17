## Default Permission

Default permission set for tauri-plugin-extensions. Grants access to every
command the plugin exposes — lifecycle, runtime, and storage surfaces.

Consumers that want a tighter allowlist should reference individual
`allow-extensions-<command>` permissions instead of `default` in their
capability file.

#### This default permission set includes the following:

- `allow-extensions-load-unpacked`
- `allow-extensions-unload`
- `allow-extensions-list`
- `allow-extensions-list-lifecycle`
- `allow-extensions-reload`
- `allow-extensions-enable`
- `allow-extensions-disable`
- `allow-extensions-reconcile-orphans`
- `allow-extensions-diagnostics`
- `allow-extensions-content-ready`
- `allow-extensions-scripting-register-content-scripts`
- `allow-extensions-scripting-unregister-content-scripts`
- `allow-extensions-scripting-get-registered-content-scripts`
- `allow-extensions-runtime-send-message`
- `allow-extensions-runtime-connect`
- `allow-extensions-runtime-port-post`
- `allow-extensions-runtime-port-disconnect`
- `allow-extensions-storage-get`
- `allow-extensions-storage-set`
- `allow-extensions-storage-remove`
- `allow-extensions-storage-clear`

## Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>


<tr>
<td>

`extensions:allow-extensions-content-ready`

</td>
<td>

Enables the extensions_content_ready command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-content-ready`

</td>
<td>

Denies the extensions_content_ready command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-diagnostics`

</td>
<td>

Enables the extensions_diagnostics command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-diagnostics`

</td>
<td>

Denies the extensions_diagnostics command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-disable`

</td>
<td>

Enables the extensions_disable command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-disable`

</td>
<td>

Denies the extensions_disable command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-enable`

</td>
<td>

Enables the extensions_enable command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-enable`

</td>
<td>

Denies the extensions_enable command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-list`

</td>
<td>

Enables the extensions_list command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-list`

</td>
<td>

Denies the extensions_list command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-list-lifecycle`

</td>
<td>

Enables the extensions_list_lifecycle command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-list-lifecycle`

</td>
<td>

Denies the extensions_list_lifecycle command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-load-unpacked`

</td>
<td>

Enables the extensions_load_unpacked command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-load-unpacked`

</td>
<td>

Denies the extensions_load_unpacked command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-reconcile-orphans`

</td>
<td>

Enables the extensions_reconcile_orphans command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-reconcile-orphans`

</td>
<td>

Denies the extensions_reconcile_orphans command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-reload`

</td>
<td>

Enables the extensions_reload command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-reload`

</td>
<td>

Denies the extensions_reload command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-runtime-connect`

</td>
<td>

Enables the extensions_runtime_connect command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-runtime-connect`

</td>
<td>

Denies the extensions_runtime_connect command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-runtime-port-disconnect`

</td>
<td>

Enables the extensions_runtime_port_disconnect command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-runtime-port-disconnect`

</td>
<td>

Denies the extensions_runtime_port_disconnect command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-runtime-port-post`

</td>
<td>

Enables the extensions_runtime_port_post command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-runtime-port-post`

</td>
<td>

Denies the extensions_runtime_port_post command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-runtime-send-message`

</td>
<td>

Enables the extensions_runtime_send_message command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-runtime-send-message`

</td>
<td>

Denies the extensions_runtime_send_message command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-scripting-get-registered-content-scripts`

</td>
<td>

Enables the extensions_scripting_get_registered_content_scripts command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-scripting-get-registered-content-scripts`

</td>
<td>

Denies the extensions_scripting_get_registered_content_scripts command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-scripting-register-content-scripts`

</td>
<td>

Enables the extensions_scripting_register_content_scripts command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-scripting-register-content-scripts`

</td>
<td>

Denies the extensions_scripting_register_content_scripts command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-scripting-unregister-content-scripts`

</td>
<td>

Enables the extensions_scripting_unregister_content_scripts command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-scripting-unregister-content-scripts`

</td>
<td>

Denies the extensions_scripting_unregister_content_scripts command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-storage-clear`

</td>
<td>

Enables the extensions_storage_clear command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-storage-clear`

</td>
<td>

Denies the extensions_storage_clear command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-storage-get`

</td>
<td>

Enables the extensions_storage_get command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-storage-get`

</td>
<td>

Denies the extensions_storage_get command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-storage-remove`

</td>
<td>

Enables the extensions_storage_remove command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-storage-remove`

</td>
<td>

Denies the extensions_storage_remove command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-storage-set`

</td>
<td>

Enables the extensions_storage_set command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-storage-set`

</td>
<td>

Denies the extensions_storage_set command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:allow-extensions-unload`

</td>
<td>

Enables the extensions_unload command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`extensions:deny-extensions-unload`

</td>
<td>

Denies the extensions_unload command without any pre-configured scope.

</td>
</tr>
</table>

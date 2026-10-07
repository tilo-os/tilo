// SPDX-License-Identifier: AGPL-3.0-only
// Runs against the pinned upstream jar, with synthetic in-memory managers.
import com.fasterxml.jackson.databind.ObjectMapper;
import java.lang.reflect.Proxy;
import java.util.List;
import org.asamk.signal.commands.ListAccountsCommand;
import org.asamk.signal.manager.Manager;
import org.asamk.signal.manager.MultiAccountManager;
import org.asamk.signal.output.JsonWriter;

public class SignalProviderIdentityFixture {
    public static void main(String[] args) throws Exception {
        var account = (Manager) Proxy.newProxyInstance(Manager.class.getClassLoader(),
                new Class<?>[]{Manager.class}, (proxy, method, arguments) -> switch (method.getName()) {
                    case "getSelfNumber" -> "+15555550101";
                    case "getSelfACI" -> "11111111-1111-4111-8111-111111111111";
                    default -> throw new AssertionError("Unexpected account operation: " + method.getName());
                });
        var managers = (MultiAccountManager) Proxy.newProxyInstance(MultiAccountManager.class.getClassLoader(),
                new Class<?>[]{MultiAccountManager.class}, (proxy, method, arguments) -> {
                    if (!method.getName().equals("getManagers")) {
                        throw new AssertionError("Unexpected manager operation: " + method.getName());
                    }
                    return List.of(account);
                });
        var json = new ObjectMapper();
        new ListAccountsCommand().handleCommand((net.sourceforge.argparse4j.inf.Namespace) null, managers, (JsonWriter) rows -> {
            try {
                var value = json.readTree(json.writeValueAsString(rows));
                if (value.size() != 1 || !value.get(0).get("number").asText().equals("+15555550101")
                        || !value.get(0).get("aci").asText().equals("11111111-1111-4111-8111-111111111111")) {
                    throw new AssertionError("Local account evidence differs");
                }
            } catch (Exception error) {
                throw new AssertionError(error);
            }
        });
        System.out.println("Pinned provider local identity fixture passed");
    }
}

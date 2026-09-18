/* Require delivery of a Vulkan debug callback across the guest/host boundary. */
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <string.h>

static const char message_id[] = "tilo.vulkan.callback";
static const char message_text[] = "Tilo Vulkan callback round trip";
static unsigned int callbacks;

static VKAPI_ATTR VkBool32 VKAPI_CALL callback(
    VkDebugUtilsMessageSeverityFlagBitsEXT severity,
    VkDebugUtilsMessageTypeFlagsEXT types,
    const VkDebugUtilsMessengerCallbackDataEXT *data, void *user_data) {
    if (severity == VK_DEBUG_UTILS_MESSAGE_SEVERITY_INFO_BIT_EXT &&
        types == VK_DEBUG_UTILS_MESSAGE_TYPE_GENERAL_BIT_EXT &&
        data && data->pMessageIdName && data->pMessage &&
        !strcmp(data->pMessageIdName, message_id) &&
        !strcmp(data->pMessage, message_text) && user_data == &callbacks) {
        ++callbacks;
    }
    return VK_FALSE;
}

int main(void) {
    const char *extension = VK_EXT_DEBUG_UTILS_EXTENSION_NAME;
    VkInstanceCreateInfo instance_info = {
        .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        .enabledExtensionCount = 1, .ppEnabledExtensionNames = &extension,
    };
    VkInstance instance;
    VkResult result = vkCreateInstance(&instance_info, NULL, &instance);
    if (result != VK_SUCCESS) {
        fprintf(stderr, "vkCreateInstance failed: %d\n", result);
        return 1;
    }
    PFN_vkCreateDebugUtilsMessengerEXT create =
        (PFN_vkCreateDebugUtilsMessengerEXT)vkGetInstanceProcAddr(instance, "vkCreateDebugUtilsMessengerEXT");
    PFN_vkDestroyDebugUtilsMessengerEXT destroy =
        (PFN_vkDestroyDebugUtilsMessengerEXT)vkGetInstanceProcAddr(instance, "vkDestroyDebugUtilsMessengerEXT");
    PFN_vkSubmitDebugUtilsMessageEXT submit =
        (PFN_vkSubmitDebugUtilsMessageEXT)vkGetInstanceProcAddr(instance, "vkSubmitDebugUtilsMessageEXT");
    if (!create || !destroy || !submit) {
        fputs("Vulkan debug-utils entry points unavailable\n", stderr);
        vkDestroyInstance(instance, NULL);
        return 1;
    }
    VkDebugUtilsMessengerCreateInfoEXT info = {
        .sType = VK_STRUCTURE_TYPE_DEBUG_UTILS_MESSENGER_CREATE_INFO_EXT,
        .messageSeverity = VK_DEBUG_UTILS_MESSAGE_SEVERITY_INFO_BIT_EXT,
        .messageType = VK_DEBUG_UTILS_MESSAGE_TYPE_GENERAL_BIT_EXT,
        .pfnUserCallback = callback, .pUserData = &callbacks,
    };
    VkDebugUtilsMessengerEXT messenger;
    result = create(instance, &info, NULL, &messenger);
    if (result != VK_SUCCESS) {
        fprintf(stderr, "vkCreateDebugUtilsMessengerEXT failed: %d\n", result);
        vkDestroyInstance(instance, NULL);
        return 1;
    }
    VkDebugUtilsMessengerCallbackDataEXT data = {
        .sType = VK_STRUCTURE_TYPE_DEBUG_UTILS_MESSENGER_CALLBACK_DATA_EXT,
        .pMessageIdName = message_id, .pMessage = message_text,
    };
    submit(instance, info.messageSeverity, info.messageType, &data);
    destroy(instance, messenger, NULL);
    vkDestroyInstance(instance, NULL);
    if (callbacks != 1) {
        fprintf(stderr, "Vulkan callback delivery failed: expected 1, received %u\n", callbacks);
        return 1;
    }
    puts("TILO_VULKAN_CALLBACK_PASS");
    return 0;
}

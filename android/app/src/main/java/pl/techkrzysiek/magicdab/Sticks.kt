package pl.techkrzysiek.magicdab

import android.content.Context
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbManager
import org.xmlpull.v1.XmlPullParser

/** The RTL-SDR sticks the app knows (res/xml/rtlsdr_devices.xml); the app opens them itself (UsbBridge). */
object Sticks {
    /** The RTL2832U sticks plugged in (listing USB devices needs no permission). */
    fun all(context: Context): List<UsbDevice> {
        val usb = context.getSystemService(UsbManager::class.java) ?: return emptyList()
        val known = ids(context)
        return usb.deviceList.values.filter { Pair(it.vendorId, it.productId) in known }.sortedBy { it.deviceName }
    }

    /** The first one. */
    fun first(context: Context): UsbDevice? = all(context).firstOrNull()

    /** Vendor and product ids of res/xml/rtlsdr_devices.xml (the same list opens the app). */
    private fun ids(context: Context): Set<Pair<Int, Int>> {
        val out = HashSet<Pair<Int, Int>>()
        val xml = context.resources.getXml(R.xml.rtlsdr_devices)
        try {
            var event = xml.eventType
            while (event != XmlPullParser.END_DOCUMENT) {
                if (event == XmlPullParser.START_TAG && xml.name == "usb-device") {
                    // (kept as text by aapt2, like Android itself reads them)
                    val vendor = xml.getAttributeValue(null, "vendor-id")?.toIntOrNull()
                    val product = xml.getAttributeValue(null, "product-id")?.toIntOrNull()
                    if (vendor != null && product != null) out.add(Pair(vendor, product))
                }
                event = xml.next()
            }
        } finally {
            xml.close()
        }
        return out
    }
}

package io.tempest.android

import android.app.Application
import io.tempest.android.data.TempestRepository

class TempestApp : Application() {
    override fun onCreate() {
        super.onCreate()
        // Warm the repository so the first screen does not pay for JNI
        // initialisation. Failures are recorded and surfaced by the UI rather
        // than crashing here.
        TempestRepository.get(this)
    }
}

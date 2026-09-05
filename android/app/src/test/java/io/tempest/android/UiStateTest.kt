package io.tempest.android

import io.tempest.android.core.Game
import io.tempest.android.ui.UiState
import io.tempest.android.ui.formatBytes
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class UiStateTest {

    private val games = listOf(
        Game(1, "Half-Life", "A first-person shooter"),
        Game(2, "Portal", null),
        Game(3, "Team Fortress", "Another shooter"),
    )

    @Test
    fun `a blank query shows everything`() {
        val state = UiState(loading = false, games = games, query = "   ")
        assertEquals(3, state.filteredGames.size)
    }

    @Test
    fun `search is case insensitive and covers descriptions`() {
        assertEquals(1, UiState(games = games, query = "PORTAL").filteredGames.size)
        assertEquals(2, UiState(games = games, query = "shooter").filteredGames.size)
        assertTrue(UiState(games = games, query = "nothing").filteredGames.isEmpty())
    }

    @Test
    fun `a game without a description is not matched by description text`() {
        // Portal has a null description; the filter must not crash on it.
        assertTrue(UiState(games = games, query = "shooter").filteredGames.none { it.id == 2 })
    }

    @Test
    fun `byte formatting picks sensible units`() {
        assertEquals("512 B", formatBytes(512))
        assertEquals("1 KB", formatBytes(1024))
        assertEquals("30 MB", formatBytes(31_457_280))
        assertEquals("1.0 GB", formatBytes(1_073_741_824))
        assertEquals("2.5 GB", formatBytes(2_684_354_560))
        assertEquals("0 B", formatBytes(0))
    }
}

package com.crayen.domovoi.ui.common

import java.time.DayOfWeek
import java.time.Instant
import java.time.LocalDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.time.temporal.ChronoUnit
import java.time.temporal.TemporalAdjusters

/** When a snooze can end, and how a wake time reads (`SnoozeTimes.swift`). */
object SnoozeTimes {
    data class Preset(val label: String, val ms: Long)

    fun presets(now: LocalDateTime = LocalDateTime.now(), zone: ZoneId = ZoneId.systemDefault()): List<Preset> {
        fun ms(t: LocalDateTime) = t.atZone(zone).toInstant().toEpochMilli()
        val out = mutableListOf(Preset("In 1 hour", ms(now.plusHours(1))))
        val evening = now.toLocalDate().atTime(18, 0)
        if (ChronoUnit.MINUTES.between(now, evening) > 60) out += Preset("This evening", ms(evening))
        val tomorrow = now.toLocalDate().plusDays(1).atTime(9, 0)
        out += Preset("Tomorrow morning", ms(tomorrow))
        val monday = now.toLocalDate().with(TemporalAdjusters.next(DayOfWeek.MONDAY)).atTime(9, 0)
        if (ChronoUnit.MINUTES.between(tomorrow, monday) > 60) out += Preset("Next week", ms(monday))
        return out
    }

    /** "18:00", "Tue 09:00", or "12 Dec" for a wake time. */
    fun label(ms: Long, now: LocalDateTime = LocalDateTime.now(), zone: ZoneId = ZoneId.systemDefault()): String {
        val t = LocalDateTime.ofInstant(Instant.ofEpochMilli(ms), zone)
        return when {
            t.toLocalDate() == now.toLocalDate() -> t.format(DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT))
            ChronoUnit.DAYS.between(now, t) < 6 -> t.format(DateTimeFormatter.ofPattern("EEE HH:mm"))
            else -> t.format(DateTimeFormatter.ofPattern("d MMM"))
        }
    }
}

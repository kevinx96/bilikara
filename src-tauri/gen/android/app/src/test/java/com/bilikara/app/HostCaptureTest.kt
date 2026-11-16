package com.bilikara.app

import java.nio.file.Files
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class HostCaptureTest {
  @Test fun nextCaptureRemovesOnlyAbandonedPrivatePhotos() {
    val directory = Files.createTempDirectory("cast-capture-test").toFile()
    try {
      val abandoned = directory.resolve("capture-old.jpg").apply { writeText("private") }
      val unrelated = directory.resolve("other.jpg").apply { writeText("keep") }
      val child = directory.resolve("capture-folder.jpg").apply { mkdir() }
      HostCapture.clearAbandonedPhotos(directory)
      assertFalse(abandoned.exists())
      assertTrue(unrelated.exists())
      assertTrue(child.isDirectory)
    } finally { directory.deleteRecursively() }
  }
}

class WidgetTest
  def test_frozen
    clock = Clock.new
    def clock.time_now
      42
    end
    clock.now
  end

  def test_other
    clock = "text"
    clock.size
  end

  def test_untyped(clock)
    def clock.tick
      1
    end
  end

  def self.helper
    :helper
  end
end

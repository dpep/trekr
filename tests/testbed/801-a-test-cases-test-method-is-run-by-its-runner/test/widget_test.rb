class WidgetTest < Minitest::Test
  def test_builds
    assert Widget.new.build
  end

  def unused_helper
  end

  private

  def test_hidden
  end
end

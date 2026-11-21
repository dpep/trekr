require "test_helper"

class HiddenTest < Toolkit::Case
  def test_value
    assert_equal 2, Hidden.new.value
  end
end

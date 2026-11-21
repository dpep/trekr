require "test_helper"

class NamedTest < ActiveSupport::TestCase
  setup :prepare

  def prepare
    @n = 2
  end

  test "value" do
    assert_equal @n, Named.new.value
  end
end

class WidgetTest < ActiveSupport::TestCase
  setup :build_widget
  teardown :drop_widget

  def build_widget
  end

  def drop_widget
  end

  def test_shiny
    assert_predicate Gadget.new, :shiny?
  end
end

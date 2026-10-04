class WidgetSerializer
  attribute :label, if: :shown?

  def shown?
    true
  end
end

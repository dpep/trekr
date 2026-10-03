class Widget
  def deconstruct_keys(keys)
    { id: 1 }
  end

  def to_a
    [1]
  end

  def lonely
    1
  end

  private

  def instance_variables_to_inspect
    [:@id]
  end
end

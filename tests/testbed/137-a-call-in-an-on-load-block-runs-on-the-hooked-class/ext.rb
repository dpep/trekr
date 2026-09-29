ActiveSupport.on_load(:active_record) do
  establish(adapter: "x")

  def touch_later
    save
  end
end

ActiveSupport.on_load(:action_controller) do
  helper :all
end

ActiveSupport.on_load(:active_record, yield: true) do |base|
  establish(adapter: "y")
end

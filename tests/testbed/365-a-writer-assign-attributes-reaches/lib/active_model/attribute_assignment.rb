module ActiveModel
  module AttributeAssignment
    def assign_attributes(new_attributes)
      new_attributes.each { |k, v| public_send("#{k}=", v) }
    end
  end
end

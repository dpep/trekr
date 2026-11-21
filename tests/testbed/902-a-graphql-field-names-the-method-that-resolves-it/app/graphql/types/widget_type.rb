module Types
  class WidgetType < GraphQL::Schema::Object
    field :display_name, String
    field :size, Integer, resolver_method: :resolve_size
    field :color, String, method: :paint

    def display_name
      "x"
    end

    def resolve_size
      1
    end

    def paint
      "red"
    end

    def full_name
    end

    def lonely
    end
  end
end

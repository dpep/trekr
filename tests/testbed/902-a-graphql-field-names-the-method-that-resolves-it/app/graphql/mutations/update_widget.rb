module Mutations
  class UpdateWidget < GraphQL::Schema::Mutation
    argument :widget_id, ID, loads: Types::WidgetType
    argument :name, String, prepare: :clean_name

    def load_widget(id)
    end

    def clean_name(value, ctx)
    end

    def lonely
    end
  end
end
